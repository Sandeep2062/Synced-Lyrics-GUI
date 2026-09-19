"""SQLite database library manager."""
import sqlite3
import threading
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

class LibraryDB:
    def __init__(self, db_path: str | Path) -> None:
        self.db_path = str(db_path)
        self._local = threading.local()
        self._init_db()

    def _get_conn(self) -> sqlite3.Connection:
        if not hasattr(self._local, "conn"):
            self._local.conn = sqlite3.connect(self.db_path, check_same_thread=False)
            self._local.conn.row_factory = sqlite3.Row
        return self._local.conn

    def _init_db(self) -> None:
        conn = self._get_conn()
        cursor = conn.cursor()
        
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS tracks (
                id INTEGER PRIMARY KEY,
                audio_path TEXT UNIQUE,
                artist TEXT,
                title TEXT,
                album TEXT,
                duration REAL,
                lrc_path TEXT,
                lrc_status TEXT,
                lyrics_source TEXT,
                last_checked REAL,
                created_at REAL
            )
        ''')
        
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS history (
                id INTEGER PRIMARY KEY,
                track_id INTEGER,
                action TEXT,
                platform TEXT,
                lrc_type TEXT,
                details TEXT,
                timestamp REAL,
                FOREIGN KEY (track_id) REFERENCES tracks(id)
            )
        ''')
        
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS cache (
                audio_path TEXT PRIMARY KEY,
                cache_timestamp REAL
            )
        ''')
        
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS directories (
                path TEXT PRIMARY KEY,
                added_at REAL
            )
        ''')
        
        cursor.execute('CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album)')
        cursor.execute('CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist)')
        cursor.execute('CREATE INDEX IF NOT EXISTS idx_tracks_status ON tracks(lrc_status)')
        
        conn.commit()

    def upsert_track(self, audio_path: str, **kwargs: Any) -> None:
        conn = self._get_conn()
        cursor = conn.cursor()
        
        cursor.execute('SELECT id FROM tracks WHERE audio_path = ?', (audio_path,))
        row = cursor.fetchone()
        
        now = time.time()
        
        if row:
            updates = []
            values = []
            for k, v in kwargs.items():
                updates.append(f"{k} = ?")
                values.append(v)
            
            if not updates:
                return
                
            query = f"UPDATE tracks SET {', '.join(updates)} WHERE audio_path = ?"
            values.append(audio_path)
            cursor.execute(query, tuple(values))
        else:
            fields = ['audio_path', 'created_at']
            values = [audio_path, now]
            for k, v in kwargs.items():
                fields.append(k)
                values.append(v)
                
            placeholders = ', '.join(['?'] * len(fields))
            query = f"INSERT INTO tracks ({', '.join(fields)}) VALUES ({placeholders})"
            cursor.execute(query, tuple(values))
            
        conn.commit()

    def get_track(self, audio_path: str) -> Optional[Dict[str, Any]]:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('SELECT * FROM tracks WHERE audio_path = ?', (audio_path,))
        row = cursor.fetchone()
        return dict(row) if row else None

    def get_all_tracks(self) -> List[Dict[str, Any]]:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('SELECT * FROM tracks')
        return [dict(row) for row in cursor.fetchall()]

    def get_tracks_by_status(self, status: str) -> List[Dict[str, Any]]:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('SELECT * FROM tracks WHERE lrc_status = ?', (status,))
        return [dict(row) for row in cursor.fetchall()]

    def get_tracks_by_album(self, album: str) -> List[Dict[str, Any]]:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('SELECT * FROM tracks WHERE album = ? ORDER BY id ASC', (album,))
        return [dict(row) for row in cursor.fetchall()]

    def get_tracks_by_artist(self, artist: str) -> List[Dict[str, Any]]:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('SELECT * FROM tracks WHERE artist = ? ORDER BY album ASC, id ASC', (artist,))
        return [dict(row) for row in cursor.fetchall()]

    def get_albums(self) -> List[Dict[str, Any]]:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('''
            SELECT 
                COALESCE(album, 'Unknown Album') as album,
                COALESCE(artist, 'Unknown Artist') as artist,
                COUNT(*) as track_count,
                SUM(CASE WHEN lrc_status = 'synced' THEN 1 ELSE 0 END) as synced_count,
                MIN(audio_path) as sample_path
            FROM tracks 
            GROUP BY album, artist
            ORDER BY album ASC
        ''')
        return [dict(row) for row in cursor.fetchall()]

    def get_artists(self) -> List[Dict[str, Any]]:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('''
            SELECT 
                COALESCE(artist, 'Unknown Artist') as artist,
                COUNT(DISTINCT album) as album_count,
                COUNT(*) as track_count,
                SUM(CASE WHEN lrc_status = 'synced' THEN 1 ELSE 0 END) as synced_count,
                MIN(audio_path) as sample_path
            FROM tracks 
            GROUP BY artist
            ORDER BY artist ASC
        ''')
        return [dict(row) for row in cursor.fetchall()]

    def close(self) -> None:
        if hasattr(self._local, "conn"):
            try:
                self._local.conn.close()
            except Exception:
                pass
            delattr(self._local, "conn")


    def add_history(self, audio_path: str, action: str, platform: str, lrc_type: str, details: str) -> None:
        conn = self._get_conn()
        cursor = conn.cursor()
        
        cursor.execute('SELECT id FROM tracks WHERE audio_path = ?', (audio_path,))
        row = cursor.fetchone()
        if not row:
            return
            
        track_id = row['id']
        cursor.execute('''
            INSERT INTO history (track_id, action, platform, lrc_type, details, timestamp)
            VALUES (?, ?, ?, ?, ?, ?)
        ''', (track_id, action, platform, lrc_type, details, time.time()))
        conn.commit()

    def get_history(self, audio_path: str) -> List[Dict[str, Any]]:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('''
            SELECT h.* FROM history h
            JOIN tracks t ON h.track_id = t.id
            WHERE t.audio_path = ?
            ORDER BY h.timestamp DESC
        ''', (audio_path,))
        return [dict(row) for row in cursor.fetchall()]

    def set_cache(self, audio_path: str) -> None:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('''
            INSERT OR REPLACE INTO cache (audio_path, cache_timestamp)
            VALUES (?, ?)
        ''', (audio_path, time.time()))
        conn.commit()

    def is_cached(self, audio_path: str, max_age_days: int) -> bool:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('SELECT cache_timestamp FROM cache WHERE audio_path = ?', (audio_path,))
        row = cursor.fetchone()
        if not row:
            return False
            
        max_age_seconds = max_age_days * 24 * 60 * 60
        return (time.time() - row['cache_timestamp']) < max_age_seconds

    def clear_cache(self) -> None:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('DELETE FROM cache')
        conn.commit()

    def add_directory(self, path: str) -> None:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('''
            INSERT OR IGNORE INTO directories (path, added_at)
            VALUES (?, ?)
        ''', (path, time.time()))
        conn.commit()

    def remove_directory(self, path: str) -> None:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('DELETE FROM directories WHERE path = ?', (path,))
        conn.commit()

    def get_directories(self) -> List[str]:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('SELECT path FROM directories')
        return [row['path'] for row in cursor.fetchall()]

    def get_stats(self) -> Dict[str, int]:
        conn = self._get_conn()
        cursor = conn.cursor()
        cursor.execute('SELECT lrc_status, COUNT(*) as count FROM tracks GROUP BY lrc_status')
        stats = {'total': 0, 'missing': 0, 'plain': 0, 'synced': 0, 'suspicious': 0}
        for row in cursor.fetchall():
            status = row['lrc_status']
            if status in stats:
                stats[status] = row['count']
            stats['total'] += row['count']
        return stats
