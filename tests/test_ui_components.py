"""Headless UI tests for SyncedLyricsGUI widgets and layouts."""
import unittest
from unittest.mock import MagicMock
import customtkinter as ctk

from app.core.art_cache import get_thumbnail
from app.ui.widgets.virtual_list import VirtualTrackList
from app.ui.widgets.track_row import TrackRow
from app.ui.widgets.now_playing_bar import NowPlayingBar
from app.ui.widgets.lyrics_drawer import LyricsDrawer
from app.ui.theme import COLORS


class TestUIWidgets(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Create a hidden window for widget instantiation tests
        cls.root = ctk.CTk()
        cls.root.withdraw()

    @classmethod
    def tearDownClass(cls):
        try:
            cls.root.quit()
            cls.root.destroy()
        except Exception:
            pass

    def test_theme_colors(self):
        self.assertEqual(COLORS["bg_primary"], "#0A0A0A")
        self.assertEqual(COLORS["bg_surface"], "#141414")
        self.assertEqual(COLORS["accent"], "#F35697")
        self.assertEqual(COLORS["pill_synced_fg"], "#4ADE80")

    def test_art_cache_placeholder(self):
        thumb = get_thumbnail("", size=(40, 40))
        self.assertIsNotNone(thumb)

    def test_virtual_list_initialization(self):
        frame = ctk.CTkFrame(self.root)
        vlist = VirtualTrackList(frame)
        self.assertIsNotNone(vlist)

        # Generate dummy tracks
        dummy_tracks = [
            {
                "audio_path": f"C:/Music/track_{i}.mp3",
                "title": f"Song {i}",
                "artist": f"Artist {i % 5}",
                "album": f"Album {i % 3}",
                "duration": 180.0 + i,
                "lrc_status": "synced" if i % 2 == 0 else "missing",
            }
            for i in range(100)
        ]

        # Feed dataset to virtual list
        vlist.set_items(dummy_tracks)
        self.assertEqual(len(vlist.items), 100)
        frame.destroy()

    def test_now_playing_and_drawer(self):
        frame = ctk.CTkFrame(self.root)
        mock_app = MagicMock()
        mock_app.config.volume = 0.7
        mock_toggle = MagicMock()
        mock_close = MagicMock()

        player = NowPlayingBar(frame, app_window=mock_app, on_toggle_lyrics=mock_toggle)
        self.assertIsNotNone(player)

        drawer = LyricsDrawer(frame, app_window=mock_app, on_close=mock_close)
        self.assertIsNotNone(drawer)

        # Test feeding lyrics to drawer
        test_lyrics = """[00:01.00]First line
[00:04.00]Second line
[00:08.00]Third line"""
        drawer.load_lyrics(test_lyrics)
        self.assertEqual(len(drawer.lines), 3)

        # Update position
        drawer.update_position(5.0)
        self.assertEqual(drawer.active_index, 1)

        frame.destroy()

    def test_virtual_album_grid(self):
        from app.ui.widgets.virtual_grid import VirtualAlbumGrid
        frame = ctk.CTkFrame(self.root)
        grid = VirtualAlbumGrid(frame)
        self.assertIsNotNone(grid)

        dummy_albums = [
            {
                "album": f"Album {i}",
                "artist": f"Artist {i % 10}",
                "track_count": 10 + (i % 5),
                "sample_path": f"C:/Music/track_{i}.mp3"
            }
            for i in range(50)
        ]
        grid.set_items(dummy_albums)
        self.assertEqual(len(grid.items), 50)
        frame.destroy()

    def test_virtual_artist_list(self):
        from app.ui.widgets.virtual_grid import VirtualArtistList
        frame = ctk.CTkFrame(self.root)
        alist = VirtualArtistList(frame)
        self.assertIsNotNone(alist)

        dummy_artists = [
            {
                "artist": f"Artist {i}",
                "track_count": 20 + i,
                "album_count": 2 + (i % 4)
            }
            for i in range(40)
        ]
        alist.set_items(dummy_artists)
        self.assertEqual(len(alist.items), 40)
        frame.destroy()


if __name__ == "__main__":
    unittest.main()

