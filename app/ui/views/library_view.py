"""LRCGET-style Tracks library browser with virtual scrolling and instant boot."""
import os
import threading
from typing import Any, List, Optional
import customtkinter as ctk

from app.ui.theme import COLORS, FONTS
from app.ui.widgets.virtual_list import VirtualTrackList
from app.core.scanner import scan_directory, TrackInfo

class LibraryView(ctk.CTkFrame):
    """
    Main Tracks browser matching LRCGET:
    - Search pill bar
    - Table column headers: Track | Duration | Lyrics
    - 60fps Virtualized Track List
    """
    def __init__(self, master: Any, app_window: Any, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_primary'], **kwargs)
        self.app_window = app_window
        self.all_tracks: List[dict] = []
        self.filtered_tracks: List[dict] = []
        
        # 1. Search & Filter Bar
        self.search_bar_frame = ctk.CTkFrame(self, fg_color="transparent", height=42)
        self.search_bar_frame.pack(fill="x", padx=16, pady=(10, 4))
        self.search_bar_frame.pack_propagate(False)
        
        self.search_var = ctk.StringVar()
        self.search_var.trace_add("write", lambda *args: self._apply_filter())
        
        self.search_entry = ctk.CTkEntry(
            self.search_bar_frame, 
            placeholder_text="🔍 Search for tracks...", 
            width=320, 
            height=34,
            corner_radius=17, # Rounded pill like LRCGET
            fg_color=COLORS['bg_input'], 
            border_color=COLORS['border'],
            textvariable=self.search_var
        )
        self.search_entry.pack(side="left")
        
        # Status Filter Pill Dropdown
        self.filter_var = ctk.StringVar(value="All Statuses")
        self.filter_dropdown = ctk.CTkOptionMenu(
            self.search_bar_frame, 
            values=["All Statuses", "Missing", "Plain", "Synced", "Suspicious"], 
            variable=self.filter_var, 
            command=lambda v: self._apply_filter(),
            fg_color=COLORS['bg_button'], 
            button_color=COLORS['border'],
            height=32,
            width=120,
            font=FONTS['small']
        )
        self.filter_dropdown.pack(side="left", padx=8)

        # Track count label on right of search bar
        self.count_lbl = ctk.CTkLabel(
            self.search_bar_frame, 
            text="0 tracks", 
            font=FONTS['small'], 
            text_color=COLORS['text_muted']
        )
        self.count_lbl.pack(side="right", padx=6)

        # 2. Table Column Headers (Matching LRCGET Screenshot 1)
        self.header_row = ctk.CTkFrame(self, fg_color="transparent", height=24)
        self.header_row.pack(fill="x", padx=20, pady=(6, 2))
        self.header_row.pack_propagate(False)
        
        ctk.CTkLabel(self.header_row, text="Track", font=FONTS['small'], text_color=COLORS['text_muted'], anchor="w").pack(side="left", padx=(50, 0))
        
        # Right headers: Actions (offset), Lyrics pill header, Duration header
        spacer = ctk.CTkLabel(self.header_row, text="", width=110)
        spacer.pack(side="right")
        
        ctk.CTkLabel(self.header_row, text="Lyrics", font=FONTS['small'], text_color=COLORS['text_muted'], width=80).pack(side="right", padx=(0, 10))
        ctk.CTkLabel(self.header_row, text="Duration", font=FONTS['small'], text_color=COLORS['text_muted'], width=60).pack(side="right", padx=(0, 10))

        # 3. 60fps Virtual Track List
        self.track_list = VirtualTrackList(
            self,
            on_play=lambda t: self.app_window.play_track(t),
            on_search=lambda t: self.app_window.download_single_track(t),
            on_click=lambda t: None
        )
        self.track_list.pack(fill="both", expand=True, padx=14, pady=(2, 6))

        # Empty state label
        self.empty_lbl = ctk.CTkLabel(
            self, 
            text="No tracks loaded.\nAdd a music folder from the top bar or Settings to automatically scan your library.",
            font=FONTS['body'],
            text_color=COLORS['text_muted']
        )
        # Not packed initially, shown if DB is empty

    def load_from_db(self):
        """Instant 0ms boot loading previously saved tracks from SQLite DB."""
        self.all_tracks = self.app_window.db.get_all_tracks()
        self._apply_filter()

    def _apply_filter(self):
        query = self.search_var.get().strip().lower()
        filter_mode = self.filter_var.get().lower().replace(" statuses", "")

        self.filtered_tracks = []
        for t in self.all_tracks:
            # Status filter
            st = t.get('lrc_status', 'missing').lower()
            if filter_mode != "all" and st != filter_mode:
                continue
            # Text query
            if query:
                title = (t.get('title') or os.path.basename(t.get('audio_path', ''))).lower()
                artist = (t.get('artist') or '').lower()
                album = (t.get('album') or '').lower()
                if query not in title and query not in artist and query not in album:
                    continue
            self.filtered_tracks.append(t)

        self.count_lbl.configure(text=f"{len(self.filtered_tracks)} tracks")
        self.track_list.set_items(self.filtered_tracks)
        
        if not self.all_tracks:
            self.empty_lbl.pack(expand=True, pady=80)
        else:
            self.empty_lbl.pack_forget()

    def trigger_auto_scan(self, directories: List[str]):
        """Scan folders in background and stream results directly into the UI."""
        if not directories:
            return
            
        self.app_window.status_bar.set_status("Scanning music library in background...")
        
        def worker():
            all_found = []
            for d in directories:
                if os.path.exists(d):
                    found = scan_directory(d, on_progress=lambda c, p: None)
                    all_found.extend(found)
                    
            # Save into DB
            for t in all_found:
                self.app_window.db.upsert_track(
                    t.audio_path,
                    artist=t.artist,
                    title=t.title,
                    album=t.album,
                    duration=t.duration,
                    lrc_path=t.lrc_path,
                    lrc_status=t.status
                )
                
            # Reload on main thread
            self.after(0, self._on_scan_completed)

        threading.Thread(target=worker, daemon=True).start()

    def _on_scan_completed(self):
        self.load_from_db()
        self.app_window.status_bar.set_status(f"Library scanned: {len(self.all_tracks)} tracks loaded.")
        if hasattr(self.app_window.views.get('albums'), 'load_albums'):
            self.app_window.views['albums'].load_albums()
        if hasattr(self.app_window.views.get('artists'), 'load_artists'):
            self.app_window.views['artists'].load_artists()
