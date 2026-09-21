use crate::model::{LyricsStatus, Track};
use crate::providers::{ProviderLyrics, ProviderName};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
}

pub struct LibraryDb {
    connection: Connection,
}

impl LibraryDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        let connection = Connection::open(path)?;
        let database = Self { connection };
        database.initialize()?;
        let _ = database.migrate_legacy_db_if_needed();
        Ok(database)
    }

    pub fn open_in_memory() -> Result<Self, RepositoryError> {
        let connection = Connection::open_in_memory()?;
        let database = Self { connection };
        database.initialize()?;
        Ok(database)
    }

    pub fn add_directory(&self, path: &str) -> Result<(), RepositoryError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO directories (path, added_at) VALUES (?1, unixepoch())",
            params![path],
        )?;
        Ok(())
    }

    pub fn directories(&self) -> Result<Vec<String>, RepositoryError> {
        let mut statement = self
            .connection
            .prepare("SELECT path FROM directories ORDER BY path COLLATE NOCASE")?;
        let paths = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(paths)
    }

    pub fn upsert_tracks(&self, tracks: &[Track]) -> Result<(), RepositoryError> {
        let transaction = self.connection.unchecked_transaction()?;
        for track in tracks {
            transaction.execute(
                r#"
                INSERT INTO tracks
                    (audio_path, artist, title, album, duration, lrc_path, lrc_status, mtime, last_checked)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(audio_path) DO UPDATE SET
                    artist = excluded.artist,
                    title = excluded.title,
                    album = excluded.album,
                    duration = excluded.duration,
                    lrc_path = excluded.lrc_path,
                    lrc_status = excluded.lrc_status,
                    mtime = excluded.mtime,
                    updated_at = unixepoch()
                "#,
                params![
                    track.audio_path,
                    track.artist,
                    track.title,
                    track.album,
                    track.duration_seconds,
                    track.lrc_path,
                    status_name(track.status),
                    track.mtime,
                    track.last_checked,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn tracks(&self) -> Result<Vec<Track>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT audio_path, lrc_path, artist, title, album, duration, lrc_status, mtime, last_checked
             FROM tracks ORDER BY album COLLATE NOCASE, title COLLATE NOCASE",
        )?;
        let tracks = statement
            .query_map([], |row| {
                Ok(Track {
                    audio_path: row.get(0)?,
                    lrc_path: row.get(1)?,
                    artist: row.get(2)?,
                    title: row.get(3)?,
                    album: row.get(4)?,
                    duration_seconds: row.get(5)?,
                    status: parse_status(&row.get::<_, String>(6)?),
                    mtime: row.get(7)?,
                    last_checked: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<Track>, _>>()?;
        Ok(tracks)
    }

    pub fn track(&self, audio_path: &str) -> Result<Option<Track>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT audio_path, lrc_path, artist, title, album, duration, lrc_status, mtime, last_checked
             FROM tracks WHERE audio_path = ?1",
        )?;
        statement
            .query_row(params![audio_path], |row| {
                Ok(Track {
                    audio_path: row.get(0)?,
                    lrc_path: row.get(1)?,
                    artist: row.get(2)?,
                    title: row.get(3)?,
                    album: row.get(4)?,
                    duration_seconds: row.get(5)?,
                    status: parse_status(&row.get::<_, String>(6)?),
                    mtime: row.get(7)?,
                    last_checked: row.get(8)?,
                })
            })
            .optional()
            .map_err(RepositoryError::from)
    }

    pub fn get_tracks_mtime_map(
        &self,
    ) -> Result<std::collections::HashMap<String, (f64, LyricsStatus)>, RepositoryError> {
        let mut statement = self
            .connection
            .prepare("SELECT audio_path, COALESCE(mtime, 0.0), lrc_status FROM tracks")?;
        let map = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (
                        row.get::<_, f64>(1)?,
                        parse_status(&row.get::<_, String>(2)?),
                    ),
                ))
            })?
            .filter_map(|result| result.ok())
            .collect();
        Ok(map)
    }

    pub fn get_tracks_map(
        &self,
    ) -> Result<std::collections::HashMap<String, Track>, RepositoryError> {
        let tracks = self.tracks()?;
        let map = tracks
            .into_iter()
            .map(|t| (t.audio_path.clone(), t))
            .collect();
        Ok(map)
    }

    pub fn delete_tracks(&self, paths: &[String]) -> Result<(), RepositoryError> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut statement = self
            .connection
            .prepare("DELETE FROM tracks WHERE audio_path = ?1")?;
        for path in paths {
            statement.execute(params![path])?;
        }
        Ok(())
    }

    pub fn update_last_checked(&self, audio_path: &str) -> Result<(), RepositoryError> {
        self.connection.execute(
            "UPDATE tracks SET last_checked = unixepoch() WHERE audio_path = ?1",
            params![audio_path],
        )?;
        Ok(())
    }

    pub fn remove_directory(&self, path: &str) -> Result<(), RepositoryError> {
        self.connection
            .execute("DELETE FROM directories WHERE path = ?1", params![path])?;
        Ok(())
    }

    pub fn add_history(
        &self,
        audio_path: &str,
        action: &str,
        provider: Option<&str>,
        details: &str,
    ) -> Result<(), RepositoryError> {
        self.connection.execute(
            "INSERT INTO history (audio_path, action, provider, details) VALUES (?1, ?2, ?3, ?4)",
            params![audio_path, action, provider, details],
        )?;
        Ok(())
    }

    pub fn update_lyrics_status(
        &self,
        audio_path: &str,
        status: LyricsStatus,
        source: Option<&str>,
    ) -> Result<(), RepositoryError> {
        self.connection.execute(
            "UPDATE tracks SET lrc_status = ?1, lyrics_source = ?2, updated_at = unixepoch()
             WHERE audio_path = ?3",
            params![status_name(status), source, audio_path],
        )?;
        Ok(())
    }

    pub fn history(&self, limit: usize) -> Result<Vec<String>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT action || ' | ' || COALESCE(provider, 'none') || ' | ' || details
             FROM history ORDER BY created_at DESC, id DESC LIMIT ?1",
        )?;
        let entries = statement
            .query_map(params![limit as i64], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(entries)
    }

    pub fn cached_lyrics(
        &self,
        audio_path: &str,
        provider: ProviderName,
    ) -> Result<Option<ProviderLyrics>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT synced, plain, confidence FROM lyrics_cache
             WHERE audio_path = ?1 AND provider = ?2 AND expires_at > unixepoch()",
        )?;
        statement
            .query_row(params![audio_path, provider.label()], |row| {
                Ok(ProviderLyrics {
                    provider,
                    synced: row.get(0)?,
                    plain: row.get(1)?,
                    confidence: row.get(2)?,
                })
            })
            .optional()
            .map_err(RepositoryError::from)
    }

    pub fn cache_lyrics(
        &self,
        audio_path: &str,
        lyrics: &ProviderLyrics,
        ttl_seconds: u64,
    ) -> Result<(), RepositoryError> {
        self.connection.execute(
            "INSERT INTO lyrics_cache
                (audio_path, provider, synced, plain, confidence, cached_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, unixepoch(), unixepoch() + ?6)
             ON CONFLICT(audio_path, provider) DO UPDATE SET
                synced = excluded.synced,
                plain = excluded.plain,
                confidence = excluded.confidence,
                cached_at = excluded.cached_at,
                expires_at = excluded.expires_at",
            params![
                audio_path,
                lyrics.provider.label(),
                lyrics.synced,
                lyrics.plain,
                lyrics.confidence,
                ttl_seconds as i64,
            ],
        )?;
        Ok(())
    }

    fn initialize(&self) -> Result<(), RepositoryError> {
        self.connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS directories (
                 path TEXT PRIMARY KEY,
                 added_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS tracks (
                 id INTEGER PRIMARY KEY,
                 audio_path TEXT NOT NULL UNIQUE,
                 artist TEXT NOT NULL,
                 title TEXT NOT NULL,
                 album TEXT NOT NULL,
                 duration REAL,
                 lrc_path TEXT NOT NULL,
                 lrc_status TEXT NOT NULL,
                 lyrics_source TEXT,
                 mtime REAL,
                 last_checked REAL,
                 created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 updated_at INTEGER NOT NULL DEFAULT (unixepoch())
             );
             CREATE INDEX IF NOT EXISTS idx_tracks_status ON tracks(lrc_status);
             CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
             CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album);
             CREATE TABLE IF NOT EXISTS history (
                 id INTEGER PRIMARY KEY,
                 audio_path TEXT NOT NULL,
                 action TEXT NOT NULL,
                 provider TEXT,
                 details TEXT,
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())
             );
             CREATE TABLE IF NOT EXISTS lyrics_cache (
                 audio_path TEXT NOT NULL,
                 provider TEXT NOT NULL,
                 synced TEXT,
                 plain TEXT,
                 confidence REAL NOT NULL,
                 cached_at INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL,
                 PRIMARY KEY (audio_path, provider)
             );",
        )?;

        // Migration: add columns if they don't exist (safe for existing DBs)
        let _ = self
            .connection
            .execute("ALTER TABLE tracks ADD COLUMN mtime REAL", []);
        let _ = self
            .connection
            .execute("ALTER TABLE tracks ADD COLUMN last_checked REAL", []);

        // Create indexes for migrated columns after ensuring columns exist
        let _ = self.connection.execute(
            "CREATE INDEX IF NOT EXISTS idx_tracks_mtime ON tracks(mtime)",
            [],
        );
        let _ = self.connection.execute(
            "CREATE INDEX IF NOT EXISTS idx_tracks_last_checked ON tracks(last_checked)",
            [],
        );

        Ok(())
    }

    pub fn migrate_legacy_db_if_needed(&self) -> Result<(), RepositoryError> {
        let dir_count: i64 = self
            .connection
            .query_row("SELECT COUNT(*) FROM directories", [], |r| r.get(0))
            .unwrap_or(0);

        if dir_count > 0 {
            return Ok(());
        }

        if let Some(legacy_dir) = crate::config::legacy_app_data_dir() {
            let legacy_db_path = legacy_dir.join("library.db");
            if legacy_db_path.is_file() {
                self.import_from_legacy_db(&legacy_db_path)?;
            }
        }
        Ok(())
    }

    pub fn import_from_legacy_db(&self, legacy_db_path: &Path) -> Result<(), RepositoryError> {
        let legacy_conn = match Connection::open_with_flags(
            legacy_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };

        // Migrate directories
        if let Ok(mut stmt) = legacy_conn.prepare("SELECT path, added_at FROM directories") {
            if let Ok(rows) = stmt.query_map([], |row| {
                let path: String = row.get(0)?;
                let added_at: f64 = row.get(1).unwrap_or(0.0);
                Ok((path, added_at as i64))
            }) {
                for entry in rows.flatten() {
                    let _ = self.connection.execute(
                        "INSERT OR IGNORE INTO directories (path, added_at) VALUES (?1, ?2)",
                        params![entry.0, entry.1],
                    );
                }
            }
        }

        // Migrate tracks
        if let Ok(mut stmt) = legacy_conn.prepare(
            "SELECT audio_path, artist, title, album, duration, lrc_path, lrc_status, lyrics_source, mtime, last_checked FROM tracks"
        ) {
            if let Ok(mut insert_stmt) = self.connection.prepare(
                "INSERT OR IGNORE INTO tracks (audio_path, artist, title, album, duration, lrc_path, lrc_status, lyrics_source, mtime, last_checked)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
            ) {
                if let Ok(rows) = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<f64>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<f64>>(8)?,
                        row.get::<_, Option<f64>>(9)?,
                    ))
                }) {
                    for track in rows.flatten() {
                        let _ = insert_stmt.execute(params![
                            track.0,
                            track.1,
                            track.2,
                            track.3,
                            track.4,
                            track.5,
                            track.6,
                            track.7,
                            track.8,
                            track.9,
                        ]);
                    }
                }
            }
        }

        Ok(())
    }
}

fn status_name(status: LyricsStatus) -> &'static str {
    match status {
        LyricsStatus::Missing => "missing",
        LyricsStatus::Plain => "plain",
        LyricsStatus::Synced => "synced",
        LyricsStatus::Suspicious => "suspicious",
    }
}

fn parse_status(status: &str) -> LyricsStatus {
    match status {
        "plain" => LyricsStatus::Plain,
        "synced" => LyricsStatus::Synced,
        "suspicious" => LyricsStatus::Suspicious,
        _ => LyricsStatus::Missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_directories_and_track_status() {
        let database = LibraryDb::open_in_memory().unwrap();
        database.add_directory("/music").unwrap();
        database
            .upsert_tracks(&[Track {
                audio_path: "/music/Artist - Song.mp3".to_string(),
                lrc_path: "/music/Artist - Song.lrc".to_string(),
                artist: "Artist".to_string(),
                title: "Song".to_string(),
                album: "Album".to_string(),
                duration_seconds: Some(180.0),
                status: LyricsStatus::Synced,
                mtime: None,
                last_checked: None,
            }])
            .unwrap();

        assert_eq!(database.directories().unwrap(), vec!["/music"]);
        assert_eq!(database.tracks().unwrap()[0].status, LyricsStatus::Synced);
        assert_eq!(database.tracks().unwrap().len(), 1);
        database
            .update_lyrics_status(
                "/music/Artist - Song.mp3",
                LyricsStatus::Plain,
                Some("Genius"),
            )
            .unwrap();
        assert_eq!(database.tracks().unwrap()[0].status, LyricsStatus::Plain);
        let lyrics = ProviderLyrics {
            provider: ProviderName::Lrclib,
            synced: Some("[00:01]cached".to_string()),
            plain: None,
            confidence: 0.9,
        };
        database
            .cache_lyrics("/music/Artist - Song.mp3", &lyrics, 3600)
            .unwrap();
        assert_eq!(
            database
                .cached_lyrics("/music/Artist - Song.mp3", ProviderName::Lrclib)
                .unwrap()
                .unwrap()
                .synced,
            lyrics.synced
        );
    }

    #[test]
    fn imports_legacy_database_records() {
        let legacy_db_path = std::env::temp_dir().join(format!(
            "legacy_lib_{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let legacy_conn = Connection::open(&legacy_db_path).unwrap();
        legacy_conn
            .execute_batch(
                "CREATE TABLE directories (path TEXT PRIMARY KEY, added_at REAL);
                 CREATE TABLE tracks (
                     id INTEGER PRIMARY KEY,
                     audio_path TEXT UNIQUE,
                     artist TEXT,
                     title TEXT,
                     album TEXT,
                     duration REAL,
                     lrc_path TEXT,
                     lrc_status TEXT,
                     lyrics_source TEXT,
                     mtime REAL,
                     last_checked REAL,
                     created_at REAL
                 );
                 INSERT INTO directories VALUES ('/legacy/music', 1700000000.0);
                 INSERT INTO tracks (audio_path, artist, title, album, duration, lrc_path, lrc_status)
                 VALUES ('/legacy/music/song.mp3', 'Legacy Artist', 'Legacy Song', 'Legacy Album', 210.0, '/legacy/music/song.lrc', 'synced');",
            )
            .unwrap();
        drop(legacy_conn);

        let new_db = LibraryDb::open_in_memory().unwrap();
        new_db.import_from_legacy_db(&legacy_db_path).unwrap();
        let _ = std::fs::remove_file(&legacy_db_path);

        let dirs = new_db.directories().unwrap();
        assert_eq!(dirs, vec!["/legacy/music".to_string()]);

        let tracks = new_db.tracks().unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].audio_path, "/legacy/music/song.mp3");
        assert_eq!(tracks[0].artist, "Legacy Artist");
        assert_eq!(tracks[0].status, LyricsStatus::Synced);
    }
}

