use crate::model::{LyricsStatus, Track};
use crate::providers::{ProviderLyrics, ProviderName};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchJobRecord {
    pub id: i64,
    pub job_type: String,
    pub mode: String,
    pub total_tracks: usize,
    pub completed_tracks: usize,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchJobItemRecord {
    pub id: i64,
    pub job_id: i64,
    pub audio_path: String,
    pub lrc_path: String,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub issue_reason: Option<String>,
    pub status: String,
    pub provider: Option<String>,
    pub score: Option<f64>,
    pub selection_reason: Option<String>,
    pub diff_preview: Option<String>,
    pub error_message: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchJobItemDraft {
    pub audio_path: String,
    pub lrc_path: String,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub issue_reason: Option<String>,
    pub diff_preview: Option<String>,
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
                    (audio_path, artist, title, album, duration, lrc_path, lrc_status, mtime, last_checked, suspicious_reason, offset_ms)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(audio_path) DO UPDATE SET
                    artist = excluded.artist,
                    title = excluded.title,
                    album = excluded.album,
                    duration = excluded.duration,
                    lrc_path = excluded.lrc_path,
                    lrc_status = excluded.lrc_status,
                    mtime = excluded.mtime,
                    suspicious_reason = excluded.suspicious_reason,
                    offset_ms = CASE WHEN excluded.offset_ms != 0 THEN excluded.offset_ms ELSE tracks.offset_ms END,
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
                    track.suspicious_reason,
                    track.lyrics_offset_ms,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn tracks(&self) -> Result<Vec<Track>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT audio_path, lrc_path, artist, title, album, duration, lrc_status, mtime, last_checked, suspicious_reason, COALESCE(offset_ms, 0)
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
                    suspicious_reason: row.get(9)?,
                    lyrics_offset_ms: row.get(10).unwrap_or(0),
                })
            })?
            .collect::<Result<Vec<Track>, _>>()?;
        Ok(tracks)
    }

    pub fn track(&self, audio_path: &str) -> Result<Option<Track>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT audio_path, lrc_path, artist, title, album, duration, lrc_status, mtime, last_checked, suspicious_reason, COALESCE(offset_ms, 0)
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
                    suspicious_reason: row.get(9)?,
                    lyrics_offset_ms: row.get(10).unwrap_or(0),
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
            "UPDATE tracks SET lrc_status = ?1, lyrics_source = ?2, suspicious_reason = NULL, updated_at = unixepoch()
             WHERE audio_path = ?3",
            params![status_name(status), source, audio_path],
        )?;
        Ok(())
    }

    pub fn update_track_metadata(
        &self,
        audio_path: &str,
        artist: &str,
        title: &str,
        album: &str,
    ) -> Result<(), RepositoryError> {
        self.connection.execute(
            "UPDATE tracks SET artist = ?1, title = ?2, album = ?3, updated_at = unixepoch()
             WHERE audio_path = ?4",
            params![artist, title, album, audio_path],
        )?;
        Ok(())
    }

    pub fn update_lyrics_status_with_reason(
        &self,
        audio_path: &str,
        status: LyricsStatus,
        source: Option<&str>,
        reason: Option<&str>,
    ) -> Result<(), RepositoryError> {
        self.connection.execute(
            "UPDATE tracks SET lrc_status = ?1, lyrics_source = ?2, suspicious_reason = ?3, updated_at = unixepoch()
             WHERE audio_path = ?4",
            params![status_name(status), source, reason, audio_path],
        )?;
        Ok(())
    }

    pub fn update_track_offset(
        &self,
        audio_path: &str,
        offset_ms: i64,
    ) -> Result<(), RepositoryError> {
        self.connection.execute(
            "UPDATE tracks SET offset_ms = ?1, updated_at = unixepoch()
             WHERE audio_path = ?2",
            params![offset_ms, audio_path],
        )?;
        Ok(())
    }

    pub fn get_track_offset(&self, audio_path: &str) -> Result<i64, RepositoryError> {
        let mut statement = self
            .connection
            .prepare("SELECT COALESCE(offset_ms, 0) FROM tracks WHERE audio_path = ?1")?;
        let offset = statement
            .query_row(params![audio_path], |row| row.get(0))
            .optional()?
            .unwrap_or(0);
        Ok(offset)
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

    pub fn create_batch_job(
        &self,
        job_type: &str,
        mode: &str,
        items: &[BatchJobItemDraft],
    ) -> Result<i64, RepositoryError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO batch_jobs (job_type, mode, total_tracks, completed_tracks, status)
             VALUES (?1, ?2, ?3, 0, 'running')",
            params![job_type, mode, items.len() as i64],
        )?;
        let job_id = transaction.last_insert_rowid();

        for item in items {
            transaction.execute(
                "INSERT INTO batch_job_items
                    (job_id, audio_path, lrc_path, artist, title, album, issue_reason, status, diff_preview)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
                params![
                    job_id,
                    item.audio_path,
                    item.lrc_path,
                    item.artist,
                    item.title,
                    item.album,
                    item.issue_reason,
                    item.diff_preview,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(job_id)
    }

    pub fn update_batch_job_status(
        &self,
        job_id: i64,
        status: &str,
    ) -> Result<(), RepositoryError> {
        self.connection.execute(
            "UPDATE batch_jobs SET status = ?1, updated_at = unixepoch() WHERE id = ?2",
            params![status, job_id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_batch_job_item_progress(
        &self,
        job_id: i64,
        audio_path: &str,
        status: &str,
        provider: Option<&str>,
        score: Option<f64>,
        selection_reason: Option<&str>,
        diff_preview: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), RepositoryError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "UPDATE batch_job_items SET
                status = ?1,
                provider = ?2,
                score = ?3,
                selection_reason = ?4,
                diff_preview = COALESCE(?5, diff_preview),
                error_message = ?6,
                updated_at = unixepoch()
             WHERE job_id = ?7 AND audio_path = ?8",
            params![
                status,
                provider,
                score,
                selection_reason,
                diff_preview,
                error,
                job_id,
                audio_path
            ],
        )?;

        transaction.execute(
            "UPDATE batch_jobs SET
                completed_tracks = (
                    SELECT COUNT(*) FROM batch_job_items
                    WHERE job_id = ?1 AND status != 'pending'
                ),
                updated_at = unixepoch()
             WHERE id = ?1",
            params![job_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn get_resumable_batch_job(&self) -> Result<Option<BatchJobRecord>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT id, job_type, mode, total_tracks, completed_tracks, status, created_at, updated_at
             FROM batch_jobs
             WHERE status IN ('running', 'paused')
             ORDER BY id DESC LIMIT 1",
        )?;
        let job = statement
            .query_row([], |row| {
                Ok(BatchJobRecord {
                    id: row.get(0)?,
                    job_type: row.get(1)?,
                    mode: row.get(2)?,
                    total_tracks: row.get::<_, i64>(3)? as usize,
                    completed_tracks: row.get::<_, i64>(4)? as usize,
                    status: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })
            .optional()?;
        Ok(job)
    }

    pub fn get_batch_job_items(
        &self,
        job_id: i64,
    ) -> Result<Vec<BatchJobItemRecord>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT id, job_id, audio_path, lrc_path, artist, title, album, issue_reason, status, provider, score, selection_reason, diff_preview, error_message, updated_at
             FROM batch_job_items
             WHERE job_id = ?1
             ORDER BY id ASC",
        )?;
        let items = statement
            .query_map(params![job_id], |row| {
                Ok(BatchJobItemRecord {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    audio_path: row.get(2)?,
                    lrc_path: row.get(3)?,
                    artist: row.get(4)?,
                    title: row.get(5)?,
                    album: row.get(6)?,
                    issue_reason: row.get(7)?,
                    status: row.get(8)?,
                    provider: row.get(9)?,
                    score: row.get(10)?,
                    selection_reason: row.get(11)?,
                    diff_preview: row.get(12)?,
                    error_message: row.get(13)?,
                    updated_at: row.get(14)?,
                })
            })?
            .collect::<Result<Vec<BatchJobItemRecord>, _>>()?;
        Ok(items)
    }

    pub fn get_pending_batch_job_items(
        &self,
        job_id: i64,
    ) -> Result<Vec<BatchJobItemRecord>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT id, job_id, audio_path, lrc_path, artist, title, album, issue_reason, status, provider, score, selection_reason, diff_preview, error_message, updated_at
             FROM batch_job_items
             WHERE job_id = ?1 AND status = 'pending'
             ORDER BY id ASC",
        )?;
        let items = statement
            .query_map(params![job_id], |row| {
                Ok(BatchJobItemRecord {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    audio_path: row.get(2)?,
                    lrc_path: row.get(3)?,
                    artist: row.get(4)?,
                    title: row.get(5)?,
                    album: row.get(6)?,
                    issue_reason: row.get(7)?,
                    status: row.get(8)?,
                    provider: row.get(9)?,
                    score: row.get(10)?,
                    selection_reason: row.get(11)?,
                    diff_preview: row.get(12)?,
                    error_message: row.get(13)?,
                    updated_at: row.get(14)?,
                })
            })?
            .collect::<Result<Vec<BatchJobItemRecord>, _>>()?;
        Ok(items)
    }

    pub fn get_latest_batch_results(
        &self,
        limit: usize,
    ) -> Result<Vec<BatchJobItemRecord>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT id, job_id, audio_path, lrc_path, artist, title, album, issue_reason, status, provider, score, selection_reason, diff_preview, error_message, updated_at
             FROM batch_job_items
             WHERE status != 'pending'
             ORDER BY updated_at DESC, id DESC
             LIMIT ?1",
        )?;
        let items = statement
            .query_map(params![limit as i64], |row| {
                Ok(BatchJobItemRecord {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    audio_path: row.get(2)?,
                    lrc_path: row.get(3)?,
                    artist: row.get(4)?,
                    title: row.get(5)?,
                    album: row.get(6)?,
                    issue_reason: row.get(7)?,
                    status: row.get(8)?,
                    provider: row.get(9)?,
                    score: row.get(10)?,
                    selection_reason: row.get(11)?,
                    diff_preview: row.get(12)?,
                    error_message: row.get(13)?,
                    updated_at: row.get(14)?,
                })
            })?
            .collect::<Result<Vec<BatchJobItemRecord>, _>>()?;
        Ok(items)
    }

    pub fn dismiss_batch_job(&self, job_id: i64) -> Result<(), RepositoryError> {
        self.connection.execute(
            "UPDATE batch_jobs SET status = 'cancelled', updated_at = unixepoch() WHERE id = ?1",
            params![job_id],
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
                 suspicious_reason TEXT,
                 offset_ms INTEGER NOT NULL DEFAULT 0,
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
             );
             CREATE TABLE IF NOT EXISTS batch_jobs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 job_type TEXT NOT NULL,
                 mode TEXT NOT NULL,
                 total_tracks INTEGER NOT NULL,
                 completed_tracks INTEGER NOT NULL DEFAULT 0,
                 status TEXT NOT NULL,
                 created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 updated_at INTEGER NOT NULL DEFAULT (unixepoch())
             );
             CREATE TABLE IF NOT EXISTS batch_job_items (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 job_id INTEGER NOT NULL REFERENCES batch_jobs(id) ON DELETE CASCADE,
                 audio_path TEXT NOT NULL,
                 lrc_path TEXT NOT NULL,
                 artist TEXT NOT NULL,
                 title TEXT NOT NULL,
                 album TEXT NOT NULL,
                 issue_reason TEXT,
                 status TEXT NOT NULL DEFAULT 'pending',
                 provider TEXT,
                 score REAL,
                 selection_reason TEXT,
                 diff_preview TEXT,
                 error_message TEXT,
                 created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 UNIQUE(job_id, audio_path)
             );
             CREATE INDEX IF NOT EXISTS idx_batch_job_items_job_status ON batch_job_items(job_id, status);",
        )?;

        // Migration: add columns if they don't exist (safe for existing DBs)
        let _ = self
            .connection
            .execute("ALTER TABLE tracks ADD COLUMN mtime REAL", []);
        let _ = self
            .connection
            .execute("ALTER TABLE tracks ADD COLUMN last_checked REAL", []);
        let _ = self
            .connection
            .execute("ALTER TABLE tracks ADD COLUMN suspicious_reason TEXT", []);
        let _ = self.connection.execute(
            "ALTER TABLE tracks ADD COLUMN offset_ms INTEGER NOT NULL DEFAULT 0",
            [],
        );

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
                suspicious_reason: None,
                lyrics_offset_ms: 0,
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
        assert_eq!(tracks[0].suspicious_reason, None);
    }

    #[test]
    fn test_suspicious_reason_persisted_and_migrated() {
        let db = LibraryDb::open_in_memory().unwrap();
        let track = Track {
            audio_path: "/music/test.mp3".to_string(),
            lrc_path: "/music/test.lrc".to_string(),
            artist: "Artist".to_string(),
            title: "Title".to_string(),
            album: "Album".to_string(),
            duration_seconds: Some(180.0),
            status: LyricsStatus::Suspicious,
            mtime: Some(100.0),
            last_checked: None,
            suspicious_reason: Some("Offset: +2.00s".to_string()),
            lyrics_offset_ms: 200,
        };
        db.upsert_tracks(std::slice::from_ref(&track)).unwrap();

        let loaded = db.track("/music/test.mp3").unwrap().unwrap();
        assert_eq!(loaded.status, LyricsStatus::Suspicious);
        assert_eq!(loaded.suspicious_reason.as_deref(), Some("Offset: +2.00s"));

        assert_eq!(loaded.lyrics_offset_ms, 200);

        db.update_track_offset("/music/test.mp3", -500).unwrap();
        assert_eq!(db.get_track_offset("/music/test.mp3").unwrap(), -500);
        let updated_offset = db.track("/music/test.mp3").unwrap().unwrap();
        assert_eq!(updated_offset.lyrics_offset_ms, -500);

        db.update_lyrics_status_with_reason(
            "/music/test.mp3",
            LyricsStatus::Suspicious,
            None,
            Some("Wrong song detected"),
        )
        .unwrap();
        let updated = db.track("/music/test.mp3").unwrap().unwrap();
        assert_eq!(
            updated.suspicious_reason.as_deref(),
            Some("Wrong song detected")
        );

        db.update_lyrics_status("/music/test.mp3", LyricsStatus::Synced, Some("lrclib"))
            .unwrap();
        let synced = db.track("/music/test.mp3").unwrap().unwrap();
        assert_eq!(synced.status, LyricsStatus::Synced);
        assert_eq!(synced.suspicious_reason, None);
        assert_eq!(synced.lyrics_offset_ms, -500);
    }

    #[test]
    fn test_batch_job_lifecycle_and_crash_resumption() {
        let db = LibraryDb::open_in_memory().unwrap();
        let items = vec![
            BatchJobItemDraft {
                audio_path: "/music/track1.mp3".into(),
                lrc_path: "/music/track1.lrc".into(),
                artist: "Artist 1".into(),
                title: "Track 1".into(),
                album: "Album 1".into(),
                issue_reason: Some("Missing".into()),
                diff_preview: None,
            },
            BatchJobItemDraft {
                audio_path: "/music/track2.mp3".into(),
                lrc_path: "/music/track2.lrc".into(),
                artist: "Artist 2".into(),
                title: "Track 2".into(),
                album: "Album 2".into(),
                issue_reason: Some("Offset: +1.20s".into()),
                diff_preview: None,
            },
        ];

        let job_id = db
            .create_batch_job("fix_safe_issues", "safe_issues", &items)
            .unwrap();

        // 1 item finishes
        db.update_batch_job_item_progress(
            job_id,
            "/music/track1.mp3",
            "completed",
            Some("LRCLIB"),
            Some(0.98),
            Some("Exact title & artist match"),
            Some("+ [00:01]line"),
            None,
        )
        .unwrap();

        // Check resumable job before simulated crash
        let resumable = db
            .get_resumable_batch_job()
            .unwrap()
            .expect("job should be resumable");
        assert_eq!(resumable.id, job_id);
        assert_eq!(resumable.total_tracks, 2);
        assert_eq!(resumable.completed_tracks, 1);
        assert_eq!(resumable.status, "running");

        // Pending items to resume
        let pending = db.get_pending_batch_job_items(job_id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].audio_path, "/music/track2.mp3");

        // Complete item 2
        db.update_batch_job_item_progress(
            job_id,
            "/music/track2.mp3",
            "completed",
            Some("Internal"),
            Some(1.0),
            Some("Offset adjusted"),
            Some("- [00:01]\n+ [00:02]"),
            None,
        )
        .unwrap();

        db.update_batch_job_status(job_id, "completed").unwrap();

        // No more resumable jobs
        assert!(db.get_resumable_batch_job().unwrap().is_none());

        // Latest result logs
        let logs = db.get_latest_batch_results(10).unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].provider.as_deref(), Some("Internal"));
        assert_eq!(logs[1].provider.as_deref(), Some("LRCLIB"));
    }
}
