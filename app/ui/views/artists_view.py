"""Artists browser view grouping music by artist with virtualized list."""
import customtkinter as ctk
from typing import Any, List, Optional
from app.ui.theme import COLORS, FONTS
from app.ui.widgets.virtual_grid import VirtualArtistList
from app.ui.widgets.track_row import TrackRow


class ArtistsView(ctk.CTkFrame):
    """View displaying list of artists with discography and tracks."""
    def __init__(self, master: Any, app_window: Any, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_primary'], **kwargs)
        self.app_window = app_window
        self.artists_data: List[dict] = []
        self.active_artist: Optional[dict] = None
        
        # Split Panes: Left = Artists List, Right = Artist Tracks
        self.grid_columnconfigure(0, weight=1)
        self.grid_columnconfigure(1, weight=2)
        self.grid_rowconfigure(0, weight=1)
        
        # Left Panel (Artists List)
        self.left_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], corner_radius=6)
        self.left_frame.grid(row=0, column=0, sticky="nsew", padx=(10, 5), pady=10)
        
        # Search Entry
        self.search_var = ctk.StringVar()
        self.search_var.trace_add("write", lambda *args: self._filter_artists())
        
        self.search_entry = ctk.CTkEntry(
            self.left_frame, 
            placeholder_text="🔍 Filter artists...", 
            fg_color=COLORS['bg_input'],
            border_color=COLORS['border'],
            textvariable=self.search_var
        )
        self.search_entry.pack(fill="x", padx=10, pady=10)
        
        # Virtualized Artist List (recycles rows)
        self.artists_list = VirtualArtistList(self.left_frame, on_select=self.select_artist)
        self.artists_list.pack(fill="both", expand=True, padx=6, pady=(0, 6))
        
        # Right Panel (Artist Tracks & Details)
        self.right_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], corner_radius=6)
        self.right_frame.grid(row=0, column=1, sticky="nsew", padx=(5, 10), pady=10)
        
        # Top banner for selected artist
        self.artist_banner = ctk.CTkFrame(self.right_frame, fg_color=COLORS['bg_toolbar'], height=60, corner_radius=6)
        self.artist_banner.pack(fill="x", padx=10, pady=10)
        self.artist_banner.pack_propagate(False)
        
        self.artist_title = ctk.CTkLabel(self.artist_banner, text="Select an artist", font=FONTS['heading'], anchor="w")
        self.artist_title.pack(side="left", padx=14)
        
        self.dl_artist_btn = ctk.CTkButton(
            self.artist_banner, 
            text="⬇ Download Artist Lyrics", 
            fg_color=COLORS['accent'], 
            hover_color=COLORS['accent_hover'],
            font=FONTS['small_bold'],
            command=self._download_artist_lyrics
        )
        self.dl_artist_btn.pack(side="right", padx=14)
        self.dl_artist_btn.pack_forget() # Hide until artist selected
        
        self.tracks_scroll = ctk.CTkScrollableFrame(self.right_frame, fg_color=COLORS['bg_secondary'])
        self.tracks_scroll.pack(fill="both", expand=True, padx=10, pady=(0, 10))

    def load_artists(self, force: bool = False):
        if not force and self.artists_data:
            return
        self.artists_data = self.app_window.db.get_artists()
        self._filter_artists()

    def _filter_artists(self):
        query = self.search_var.get().strip().lower()
        filtered = [
            a for a in self.artists_data 
            if not query or query in a.get('artist', '').lower()
        ]
        self.artists_list.set_items(filtered)

    def select_artist(self, artist_info: dict):
        self.active_artist = artist_info
        name = artist_info.get('artist', 'Unknown Artist')
        self.artist_title.configure(text=f"{name} ({artist_info.get('track_count', 0)} tracks)")
        self.dl_artist_btn.pack(side="right", padx=14)

        tracks = self.app_window.db.get_tracks_by_artist(name)
        for w in self.tracks_scroll.winfo_children():
            w.destroy()

        for t in tracks:
            row = TrackRow(
                self.tracks_scroll,
                track_data=t,
                on_play=lambda track=t: self.app_window.play_track(track),
                on_search=lambda track=t: self.app_window.download_single_track(track)
            )
            row.pack(fill="x", pady=2)

    def _download_artist_lyrics(self):
        if not self.active_artist:
            return
        name = self.active_artist.get('artist')
        tracks = self.app_window.db.get_tracks_by_artist(name)
        from app.core.scanner import TrackInfo
        track_objs = [
            TrackInfo(
                audio_path=t['audio_path'],
                lrc_path=t['lrc_path'],
                artist=t['artist'],
                title=t['title'],
                album=t['album'],
                duration=t['duration'],
                status=t['lrc_status']
            )
            for t in tracks
        ]
        self.app_window.start_batch_download(track_objs, mode="smart")
