"""Albums grid browser with album-level lyrics downloading and virtualized grid."""
import customtkinter as ctk
from typing import Any, List, Optional
from app.ui.theme import COLORS, FONTS
from app.core.art_cache import load_thumbnail_async
from app.ui.widgets.virtual_grid import VirtualAlbumGrid
from app.ui.widgets.track_row import TrackRow


class AlbumsView(ctk.CTkFrame):
    """View displaying all music albums with instant virtualized grid."""
    def __init__(self, master: Any, app_window: Any, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_primary'], **kwargs)
        self.app_window = app_window
        self.albums_data: List[dict] = []
        self.active_album: Optional[dict] = None
        
        # 1. Main Container (Switches between Grid and Album Detail)
        self.main_container = ctk.CTkFrame(self, fg_color=COLORS['bg_primary'])
        self.main_container.pack(fill="both", expand=True)
        
        self._build_grid_view()
        self._build_detail_view()
        self.show_grid()

    def _build_grid_view(self):
        self.grid_frame = ctk.CTkFrame(self.main_container, fg_color=COLORS['bg_primary'])
        
        # Search bar for albums
        self.search_var = ctk.StringVar()
        self.search_var.trace_add("write", lambda *args: self._filter_albums())
        
        self.search_frame = ctk.CTkFrame(self.grid_frame, fg_color="transparent")
        self.search_frame.pack(fill="x", padx=16, pady=10)
        
        self.search_entry = ctk.CTkEntry(
            self.search_frame, 
            placeholder_text="🔍 Filter albums by name or artist...", 
            width=320, 
            fg_color=COLORS['bg_input'], 
            border_color=COLORS['border'],
            textvariable=self.search_var
        )
        self.search_entry.pack(side="left")
        
        self.album_count_lbl = ctk.CTkLabel(
            self.search_frame, 
            text="0 albums", 
            font=FONTS['body'], 
            text_color=COLORS['text_muted']
        )
        self.album_count_lbl.pack(side="right", padx=10)

        # High-performance 60fps Virtual Album Grid
        self.album_grid = VirtualAlbumGrid(self.grid_frame, on_open=self.open_album)
        self.album_grid.pack(fill="both", expand=True, padx=10, pady=4)

    def _build_detail_view(self):
        self.detail_frame = ctk.CTkFrame(self.main_container, fg_color=COLORS['bg_primary'])
        
        # Top banner with back button
        self.banner = ctk.CTkFrame(self.detail_frame, fg_color=COLORS['bg_toolbar'], height=130, corner_radius=6)
        self.banner.pack(fill="x", padx=10, pady=10)
        self.banner.pack_propagate(False)
        
        self.detail_cover = ctk.CTkLabel(self.banner, text="", width=110, height=110)
        self.detail_cover.pack(side="left", padx=12, pady=10)
        
        info_col = ctk.CTkFrame(self.banner, fg_color="transparent")
        info_col.pack(side="left", fill="both", expand=True, padx=10, pady=10)
        
        self.back_btn = ctk.CTkButton(
            info_col, 
            text="← Back to Albums", 
            width=110, 
            height=24,
            fg_color=COLORS['bg_button'],
            hover_color=COLORS['bg_button_hover'],
            font=FONTS['small_bold'],
            command=self.show_grid
        )
        self.back_btn.pack(anchor="w", pady=(0, 4))
        
        self.detail_title = ctk.CTkLabel(info_col, text="", font=FONTS['heading'], text_color=COLORS['text_primary'], anchor="w")
        self.detail_title.pack(fill="x")
        
        self.detail_sub = ctk.CTkLabel(info_col, text="", font=FONTS['body'], text_color=COLORS['text_secondary'], anchor="w")
        self.detail_sub.pack(fill="x")
        
        # Action buttons on right of banner
        actions_col = ctk.CTkFrame(self.banner, fg_color="transparent")
        actions_col.pack(side="right", padx=16, pady=10)
        
        self.dl_album_btn = ctk.CTkButton(
            actions_col, 
            text="⬇ Download Album Lyrics", 
            fg_color=COLORS['accent'], 
            hover_color=COLORS['accent_hover'],
            font=FONTS['small_bold'],
            command=self._download_album_lyrics
        )
        self.dl_album_btn.pack(pady=4)
        
        # Scrollable track list in detail view
        self.album_tracks_scroll = ctk.CTkScrollableFrame(self.detail_frame, fg_color=COLORS['bg_primary'])
        self.album_tracks_scroll.pack(fill="both", expand=True, padx=10, pady=(0, 10))

    def load_albums(self, force: bool = False):
        """Fetch albums from database and populate grid."""
        if not force and self.albums_data:
            return
        self.albums_data = self.app_window.db.get_albums()
        self._filter_albums()

    def _filter_albums(self):
        query = self.search_var.get().strip().lower()
        filtered = [
            a for a in self.albums_data 
            if not query or query in a.get('album', '').lower() or query in a.get('artist', '').lower()
        ]
        self.album_count_lbl.configure(text=f"{len(filtered)} albums")
        self.album_grid.set_items(filtered)

    def open_album(self, album_info: dict):
        self.active_album = album_info
        album_name = album_info.get('album', 'Unknown Album')
        artist_name = album_info.get('artist', 'Unknown Artist')
        sample_path = album_info.get('sample_path', '')
        
        self.detail_title.configure(text=album_name)
        self.detail_sub.configure(text=f"{artist_name} • {album_info.get('track_count', 0)} tracks")
        
        # Load cover asynchronously (checks embedded art, then folder cover.jpg)
        load_thumbnail_async(sample_path, size=(110, 110), target_widget=self.detail_cover)
        
        # Load tracks for this album (clean layout with track numbers 01, 02... matching LRCGET)
        tracks = self.app_window.db.get_tracks_by_album(album_name)
        for w in self.album_tracks_scroll.winfo_children():
            w.destroy()
            
        for idx, t in enumerate(tracks, 1):
            row = TrackRow(
                self.album_tracks_scroll,
                track_data=t,
                show_cover=False,
                track_index=idx,
                on_play=lambda track=t: self.app_window.play_track(track),
                on_search=lambda track=t: self.app_window.download_single_track(track)
            )
            row.pack(fill="x", pady=2)
            
        self.show_detail()

    def _download_album_lyrics(self):
        if not self.active_album:
            return
        album_name = self.active_album.get('album')
        tracks = self.app_window.db.get_tracks_by_album(album_name)
        if not tracks:
            return
            
        self.dl_album_btn.configure(text="⏳ Downloading...")
        self.after(1000, lambda: self.dl_album_btn.configure(text="⬇ Download Album Lyrics"))
        
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

    def show_grid(self):
        self.detail_frame.pack_forget()
        self.grid_frame.pack(fill="both", expand=True)

    def show_detail(self):
        self.grid_frame.pack_forget()
        self.detail_frame.pack(fill="both", expand=True)
