"""LRCGET-style track row widget with artwork thumbnail, status pill, and action buttons."""
import os
import subprocess
import tkinter as tk
import customtkinter as ctk
from typing import Callable, Any, Optional

from app.ui.theme import COLORS, FONTS, get_status_colors
from app.core.lrc_utils import fmt_duration
from app.core.art_cache import get_thumbnail, load_thumbnail_async


def _get_val(obj: Any, key: str, default: Any = None) -> Any:
    if isinstance(obj, dict):
        return obj.get(key, default)
    return getattr(obj, key, default)

class TrackRow(ctk.CTkFrame):
    """
    Track row matching LRCGET's layout:
    [Thumb] [Title / Artist • Album] [Duration] [Status Pill] [▶] [🔍] [⋮]
    Designed to be recyclable for 60fps virtualized scrolling.
    """
    ROW_HEIGHT = 56

    def __init__(
        self,
        master: Any,
        track_data: Optional[Any] = None,
        on_play: Optional[Callable] = None,
        on_search: Optional[Callable] = None,
        on_click: Optional[Callable] = None,
        **kwargs
    ):
        super().__init__(
            master, 
            fg_color=COLORS['bg_secondary'], 
            height=self.ROW_HEIGHT, 
            corner_radius=6, 
            border_width=0,
            **kwargs
        )
        self.pack_propagate(False)
        self.track_data = track_data
        self.on_play = on_play
        self.on_search = on_search
        self.on_click = on_click
        
        # 1. Artwork Thumbnail
        self.thumb_lbl = ctk.CTkLabel(self, text="", width=40, height=40)
        self.thumb_lbl.pack(side="left", padx=(10, 10), pady=8)
        
        # 2. Details (Title in bold, Artist • Album in muted)
        self.info_frame = ctk.CTkFrame(self, fg_color="transparent")
        self.info_frame.pack(side="left", fill="both", expand=True, pady=6)
        
        self.title_lbl = ctk.CTkLabel(
            self.info_frame, 
            text="", 
            font=FONTS['body_bold'], 
            text_color=COLORS['text_primary'], 
            anchor="w"
        )
        self.title_lbl.pack(fill="x")
        
        self.subtitle_lbl = ctk.CTkLabel(
            self.info_frame, 
            text="", 
            font=FONTS['small'], 
            text_color=COLORS['text_secondary'], 
            anchor="w"
        )
        self.subtitle_lbl.pack(fill="x")
        
        # 3. Actions on right
        self.actions_frame = ctk.CTkFrame(self, fg_color="transparent")
        self.actions_frame.pack(side="right", padx=(0, 10))
        
        self.menu_btn = ctk.CTkButton(
            self.actions_frame, 
            text="⋮", 
            width=28, 
            height=28,
            fg_color="transparent", 
            text_color=COLORS['text_muted'], 
            hover_color=COLORS['bg_button'],
            font=FONTS['heading'],
            command=self._show_context_menu
        )
        self.menu_btn.pack(side="right", padx=2)
        
        self.search_btn = ctk.CTkButton(
            self.actions_frame, 
            text="🔍", 
            width=28, 
            height=28,
            fg_color="transparent", 
            text_color=COLORS['text_muted'], 
            hover_color=COLORS['bg_button'],
            font=FONTS['small'],
            command=self._trigger_search
        )
        self.search_btn.pack(side="right", padx=2)
        
        self.play_btn = ctk.CTkButton(
            self.actions_frame, 
            text="▶", 
            width=28, 
            height=28,
            fg_color="transparent", 
            text_color=COLORS['text_primary'], 
            hover_color=COLORS['bg_button'],
            font=FONTS['small_bold'],
            command=self._trigger_play
        )
        self.play_btn.pack(side="right", padx=2)

        # 4. Status Pill
        self.pill_frame = ctk.CTkFrame(self, corner_radius=12, height=24)
        self.pill_frame.pack(side="right", padx=(10, 14))
        self.pill_lbl = ctk.CTkLabel(self.pill_frame, text="", font=FONTS['small_bold'])
        self.pill_lbl.pack(padx=10, pady=2)
        
        # 5. Duration
        self.duration_lbl = ctk.CTkLabel(
            self, 
            text="--:--", 
            font=FONTS['mono'], 
            text_color=COLORS['text_muted'], 
            width=50,
            anchor="e"
        )
        self.duration_lbl.pack(side="right", padx=(4, 10))

        # Hover & Click events
        self.bind("<Enter>", self._on_enter)
        self.bind("<Leave>", self._on_leave)
        self.bind("<Button-1>", self._on_row_click)
        self.bind("<Double-Button-1>", lambda e: self._trigger_play())
        self.bind("<Button-3>", lambda e: self._show_context_menu(e))
        
        for widget in (self.thumb_lbl, self.info_frame, self.title_lbl, self.subtitle_lbl):
            widget.bind("<Enter>", self._on_enter)
            widget.bind("<Leave>", self._on_leave)
            widget.bind("<Button-1>", self._on_row_click)
            widget.bind("<Double-Button-1>", lambda e: self._trigger_play())
            widget.bind("<Button-3>", lambda e: self._show_context_menu(e))

        if track_data:
            self.update_data(track_data)

    def update_data(self, track_data: Any):
        """Recycle row with new data without destroying widgets."""
        self.track_data = track_data
        
        # Title
        title = _get_val(track_data, 'title')
        audio_path = _get_val(track_data, 'audio_path', '')
        if not title and audio_path:
            title = os.path.splitext(os.path.basename(audio_path))[0]
        self.title_lbl.configure(text=title or "Unknown Title")
        
        # Artist • Album
        artist = _get_val(track_data, 'artist') or "Unknown Artist"
        album = _get_val(track_data, 'album') or "Unknown Album"
        self.subtitle_lbl.configure(text=f"{artist} • {album}")
        
        # Duration
        dur = _get_val(track_data, 'duration')
        if isinstance(dur, (int, float)) and dur > 0:
            self.duration_lbl.configure(text=fmt_duration(dur))
        elif isinstance(dur, str):
            self.duration_lbl.configure(text=dur)
        else:
            self.duration_lbl.configure(text="--:--")
            
        # Status Pill
        status = _get_val(track_data, 'status') or _get_val(track_data, 'lrc_status', 'missing')
        status_clean = (status or 'missing').capitalize()
        bg_col, fg_col = get_status_colors(status)
        
        self.pill_frame.configure(fg_color=bg_col)
        self.pill_lbl.configure(text=status_clean, text_color=fg_col)
        
        # Thumbnail (extracted asynchronously in background thread)
        load_thumbnail_async(audio_path, size=(40, 40), target_widget=self.thumb_lbl)


    def _trigger_play(self):
        if self.on_play and self.track_data:
            self.on_play(self.track_data)

    def _trigger_search(self):
        if self.on_search and self.track_data:
            self.on_search(self.track_data)

    def _on_row_click(self, event):
        if self.on_click and self.track_data:
            self.on_click(self.track_data)

    def _on_enter(self, event):
        self.configure(fg_color=COLORS['bg_hover'])

    def _on_leave(self, event):
        self.configure(fg_color=COLORS['bg_secondary'])

    def _show_context_menu(self, event=None):
        menu = tk.Menu(self, tearoff=0, bg=COLORS['bg_toolbar'], fg=COLORS['text_primary'], activebackground=COLORS['accent'])
        menu.add_command(label="▶ Play Track", command=self._trigger_play)
        menu.add_command(label="🔍 Search Lyrics Online", command=self._trigger_search)
        menu.add_command(label="📁 Open in File Explorer", command=self._open_folder)
        
        if event:
            x, y = event.x_root, event.y_root
        else:
            x = self.menu_btn.winfo_rootx()
            y = self.menu_btn.winfo_rooty() + 28
        menu.tk_popup(x, y)

    def _open_folder(self):
        path = _get_val(self.track_data, 'audio_path')
        if path and os.path.exists(path):
            folder = os.path.dirname(path)
            try:
                if os.name == 'nt':
                    os.startfile(folder)
                else:
                    subprocess.Popen(['xdg-open', folder])
            except Exception:
                pass
