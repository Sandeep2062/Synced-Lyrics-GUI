"""Albums grid browser with album-level lyrics downloading and track lists."""
import customtkinter as ctk
from typing import Any, List, Optional
from app.ui.theme import COLORS, FONTS
from app.core.art_cache import get_thumbnail
from app.ui.widgets.track_row import TrackRow

class AlbumCard(ctk.CTkFrame):
    """Visual album card with 120x120 artwork cover, title, artist, and track count."""
    def __init__(self, master: Any, album_info: dict, on_open: Any, **kwargs):
        super().__init__(
            master, 
            fg_color=COLORS['bg_secondary'], 
            corner_radius=8, 
            width=160, 
            height=210, 
            **kwargs
        )
        self.pack_propagate(False)
        self.album_info = album_info
        self.on_open = on_open
        
        album_name = album_info.get('album', 'Unknown Album')
        artist_name = album_info.get('artist', 'Unknown Artist')
        count = album_info.get('track_count', 0)
        sample_path = album_info.get('sample_path', '')
        
        # Album Art
        self.art_lbl = ctk.CTkLabel(self, text="", width=140, height=140)
        self.art_lbl.pack(padx=10, pady=(10, 4))
        
        # Load cover
        thumb = get_thumbnail(sample_path, size=(140, 140))
        self.art_lbl.configure(image=thumb)
        
        # Title
        self.title_lbl = ctk.CTkLabel(
            self, 
            text=album_name, 
            font=FONTS['body_bold'], 
            text_color=COLORS['text_primary'],
            anchor="w"
        )
        self.title_lbl.pack(fill="x", padx=10)
        
        # Subtitle
        self.sub_lbl = ctk.CTkLabel(
            self, 
            text=f"{artist_name} • {count} tracks", 
            font=FONTS['small'], 
            text_color=COLORS['text_muted'],
            anchor="w"
        )
        self.sub_lbl.pack(fill="x", padx=10, pady=(0, 6))

        # Events
        self.bind("<Enter>", lambda e: self.configure(fg_color=COLORS['bg_hover']))
        self.bind("<Leave>", lambda e: self.configure(fg_color=COLORS['bg_secondary']))
        self.bind("<Button-1>", lambda e: self.on_open(self.album_info))
        
        for w in (self.art_lbl, self.title_lbl, self.sub_lbl):
            w.bind("<Enter>", lambda e: self.configure(fg_color=COLORS['bg_hover']))
            w.bind("<Leave>", lambda e: self.configure(fg_color=COLORS['bg_secondary']))
            w.bind("<Button-1>", lambda e: self.on_open(self.album_info))

class AlbumsView(ctk.CTkFrame):
    """View displaying all music albums with cover thumbnails and album details."""
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

        # Scrollable grid frame
        self.scroll_grid = ctk.CTkScrollableFrame(self.grid_frame, fg_color=COLORS['bg_primary'])
        self.scroll_grid.pack(fill="both", expand=True, padx=10, pady=4)

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

    def load_albums(self):
        """Fetch albums from database and populate grid."""
        self.albums_data = self.app_window.db.get_albums()
        self._filter_albums()

    def _filter_albums(self):
        query = self.search_var.get().strip().lower()
        for w in self.scroll_grid.winfo_children():
            w.destroy()

        filtered = [
            a for a in self.albums_data 
            if not query or query in a.get('album', '').lower() or query in a.get('artist', '').lower()
        ]
        
        self.album_count_lbl.configure(text=f"{len(filtered)} albums")
        
        if not filtered:
            lbl = ctk.CTkLabel(
                self.scroll_grid, 
                text="No albums found in your music library.\nAdd folders in Settings to populate albums.",
                font=FONTS['body'],
                text_color=COLORS['text_muted']
            )
            lbl.pack(pady=60)
            return

        # Render in a responsive grid using a flow frame layout
        row_frame = None
        cards_per_row = 5 # default approximate
        
        for idx, album in enumerate(filtered):
            if idx % cards_per_row == 0:
                row_frame = ctk.CTkFrame(self.scroll_grid, fg_color="transparent")
                row_frame.pack(fill="x", pady=6)
                
            card = AlbumCard(row_frame, album_info=album, on_open=self.open_album)
            card.pack(side="left", padx=8)

    def open_album(self, album_info: dict):
        self.active_album = album_info
        album_name = album_info.get('album', 'Unknown Album')
        artist_name = album_info.get('artist', 'Unknown Artist')
        sample_path = album_info.get('sample_path', '')
        
        self.detail_title.configure(text=album_name)
        self.detail_sub.configure(text=f"{artist_name} • {album_info.get('track_count', 0)} tracks")
        
        # Load cover
        cover = get_thumbnail(sample_path, size=(110, 110))
        self.detail_cover.configure(image=cover)
        
        # Load tracks for this album
        tracks = self.app_window.db.get_tracks_by_album(album_name)
        for w in self.album_tracks_scroll.winfo_children():
            w.destroy()
            
        for t in tracks:
            row = TrackRow(
                self.album_tracks_scroll,
                track_data=t,
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
