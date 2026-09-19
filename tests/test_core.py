"""Unit tests for SyncedLyricsGUI core functionality using Python standard unittest."""
import unittest
import tempfile
from pathlib import Path

from app.core.lrc_utils import (
    parse_stamps,
    count_timestamps,
    is_synced,
    is_plain,
    normalize_title,
    titles_match,
    fmt_duration,
)
from app.core.library_db import LibraryDB
from app.config import Config
from app.api.base import RateLimitConfig, AdaptiveRateLimiter


class TestCore(unittest.TestCase):
    def test_lrc_utils_timestamps(self):
        lrc_text = """[ti:Shape of You]
[ar:Ed Sheeran]
[00:05.12]The club isn't the best place to find a lover
[00:08.45]So the bar is where I go
[00:11.89]Me and my friends at the table doing shots
"""
        stamps = parse_stamps(lrc_text)
        self.assertEqual(len(stamps), 3)
        self.assertAlmostEqual(stamps[0], 5.12, places=2)
        self.assertAlmostEqual(stamps[1], 8.45, places=2)
        self.assertAlmostEqual(stamps[2], 11.89, places=2)
        self.assertTrue(is_synced(lrc_text))
        self.assertFalse(is_plain(lrc_text))

    def test_lrc_utils_plain(self):
        plain_text = "Just some text\nWithout timestamps\nAnother line"
        self.assertFalse(is_synced(plain_text))
        self.assertTrue(is_plain(plain_text))
        self.assertEqual(count_timestamps(plain_text), 0)

    def test_lrc_title_normalization(self):
        self.assertEqual(normalize_title("Song Title (feat. Artist)"), "songtitle")
        self.assertEqual(normalize_title("Track Name - Remastered 2020"), "trackname")
        self.assertTrue(titles_match("Shape of You (Official)", "Shape of You"))
        self.assertEqual(fmt_duration(195.5), "3:15")

    def test_library_db_operations(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            db_file = Path(tmpdir) / "test_library.db"
            db = LibraryDB(db_file)
            try:
                # Upsert tracks
                db.upsert_track(
                    audio_path="C:/Music/song1.mp3",
                    artist="Queen",
                    title="Bohemian Rhapsody",
                    album="A Night at the Opera",
                    duration=354.0,
                    lrc_status="synced",
                )

                db.upsert_track(
                    audio_path="C:/Music/song2.mp3",
                    artist="Queen",
                    title="Love of My Life",
                    album="A Night at the Opera",
                    duration=218.0,
                    lrc_status="missing",
                )

                db.upsert_track(
                    audio_path="C:/Music/song3.mp3",
                    artist="Adele",
                    title="Hello",
                    album="25",
                    duration=295.0,
                    lrc_status="synced",
                )

                # Verify track retrieval
                t1 = db.get_track("C:/Music/song1.mp3")
                self.assertIsNotNone(t1)
                self.assertEqual(t1["artist"], "Queen")
                self.assertEqual(t1["lrc_status"], "synced")

                # Verify albums
                albums = db.get_albums()
                self.assertEqual(len(albums), 2)
                opera_album = next(a for a in albums if a["album"] == "A Night at the Opera")
                self.assertEqual(opera_album["artist"], "Queen")
                self.assertEqual(opera_album["track_count"], 2)
                self.assertEqual(opera_album["synced_count"], 1)

                # Verify artists
                artists = db.get_artists()
                self.assertEqual(len(artists), 2)
                queen = next(a for a in artists if a["artist"] == "Queen")
                self.assertEqual(queen["track_count"], 2)
                self.assertEqual(queen["album_count"], 1)

                # Verify tracks by album
                opera_tracks = db.get_tracks_by_album("A Night at the Opera")
                self.assertEqual(len(opera_tracks), 2)

                # Verify stats
                stats = db.get_stats()
                self.assertEqual(stats.get("synced", 0), 2)
                self.assertEqual(stats.get("missing", 0), 1)
            finally:
                db.close()


    def test_adaptive_rate_limiter(self):
        config = RateLimitConfig(
            requests_per_minute=5,
            requests_per_day=10,
            base_interval=0.1,
            max_backoff=5.0,
            retry_count=2,
        )
        limiter = AdaptiveRateLimiter(config)
        self.assertFalse(limiter.is_daily_exhausted())

        # Simulate penalty
        limiter.penalize()
        self.assertGreater(limiter.get_retry_after(), 0)

        # Simulate relaxation
        limiter.relax()


if __name__ == "__main__":
    unittest.main()
