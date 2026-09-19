"""LRCGET slide-up lyrics viewer with karaoke highlighting and copy button."""
import re
import tkinter as tk
import customtkinter as ctk
from typing import Any, List, Dict, Optional

from app.ui.theme import COLORS, FONTS
from app.core.lrc_utils import STAMP_RE

class LyricsDrawer(ctk.CTkFrame):
    """
    Slide-up synchronized lyrics drawer matching LRCGET screenshot 2:
    - Large centered lyrics
    - Active line highlighted in bright white/accent
    - Past/future lines dimmed
    - Click-to-seek
    - '📋 Copy' button
    - Close '✕' button
    """
    def __init__(self, master: Any, app_window: Any, on_close: Any, **kwargs):
        super().__init__(
            master, 
            fg_color=COLORS['bg_primary'], 
            border_width=1, 
            border_color=COLORS['border'], 
            corner_radius=0,
            **kwargs
        )
        self.app_window = app_window
        self.on_close = on_close
        
        self.lines: List[Dict[str, Any]] = []
        self.labels: List[ctk.CTkLabel] = []
        self.active_index: int = -1
        self.is_synced: bool = False
        self._raw_text: str = ""
        
        # Header / Drag Handle Row
        self.header = ctk.CTkFrame(self, fg_color="transparent", height=36)
        self.header.pack(fill="x", padx=16, pady=(6, 0))
        self.header.pack_propagate(False)
        
        # Centered handle dots
        self.handle_lbl = ctk.CTkLabel(self.header, text="••••••", font=FONTS['mono'], text_color=COLORS['text_muted'])
        self.handle_lbl.pack(side="left", expand=True)
        
        # Close button
        self.close_btn = ctk.CTkButton(
            self.header, 
            text="✕", 
            width=28, 
            height=28, 
            fg_color="transparent", 
            hover_color=COLORS['bg_button'],
            text_color=COLORS['text_muted'],
            command=self.on_close
        )
        self.close_btn.pack(side="right")
        
        # Scrollable centered lyrics container
        self.scroll = ctk.CTkScrollableFrame(self, fg_color=COLORS['bg_primary'])
        self.scroll.pack(fill="both", expand=True, padx=20, pady=(0, 10))
        
        # Copy button floating in bottom right
        self.bottom_bar = ctk.CTkFrame(self, fg_color="transparent", height=36)
        self.bottom_bar.pack(fill="x", padx=16, pady=(0, 6))
        
        self.copy_btn = ctk.CTkButton(
            self.bottom_bar, 
            text="📋 Copy", 
            width=70, 
            height=28,
            fg_color=COLORS['bg_button'], 
            hover_color=COLORS['bg_button_hover'],
            text_color=COLORS['text_primary'],
            font=FONTS['small_bold'],
            command=self._copy_lyrics
        )
        self.copy_btn.pack(side="right")
        
        self.placeholder = ctk.CTkLabel(
            self.scroll, 
            text="Select a song to display synchronized lyrics", 
            font=FONTS['lyrics'], 
            text_color=COLORS['text_muted']
        )
        self.placeholder.pack(expand=True, pady=100)

    def load_lyrics(self, lrc_text: str):
        self._raw_text = lrc_text or ""
        
        # Clear existing
        for lbl in self.labels:
            lbl.destroy()
        self.labels.clear()
        self.lines.clear()
        self.active_index = -1
        self.is_synced = False
        
        if self.placeholder:
            self.placeholder.destroy()
            self.placeholder = None

        if not lrc_text or not lrc_text.strip():
            self.placeholder = ctk.CTkLabel(
                self.scroll, 
                text="No lyrics available for this track", 
                font=FONTS['lyrics'], 
                text_color=COLORS['text_muted']
            )
            self.placeholder.pack(expand=True, pady=100)
            return

        # Parse lines
        synced_entries = []
        plain_entries = []
        
        for line in lrc_text.strip().splitlines():
            line_str = line.strip()
            if not line_str:
                continue
            matches = STAMP_RE.findall(line_str)
            if matches:
                text = STAMP_RE.sub('', line_str).strip()
                for m, s, f in matches:
                    t = int(m) * 60 + int(s)
                    if f:
                        t += int(f) / (10 ** len(f))
                    synced_entries.append({"time": t, "text": text})
            else:
                if re.match(r"^\[[a-zA-Z]+:.*\]$", line_str):
                    continue
                plain_entries.append({"time": -1.0, "text": line_str})

        if synced_entries:
            synced_entries.sort(key=lambda x: x["time"])
            self.lines = synced_entries
            self.is_synced = True
        else:
            self.lines = plain_entries
            self.is_synced = False

        # Add initial top padding for centered look
        top_pad = ctk.CTkLabel(self.scroll, text="", height=40)
        top_pad.pack()
        self.labels.append(top_pad)

        for item in self.lines:
            lbl = ctk.CTkLabel(
                self.scroll, 
                text=item['text'] if item['text'] else "♪", 
                font=FONTS['lyrics'],
                text_color=COLORS['text_muted'],
                wraplength=700,
                justify="center",
                cursor="hand2" if self.is_synced else "arrow"
            )
            lbl.pack(pady=8, anchor="center")
            
            if self.is_synced:
                t_val = item['time']
                lbl.bind("<Button-1>", lambda e, t=t_val: self._seek_to(t))
                
            self.labels.append(lbl)

        # Bottom padding
        bot_pad = ctk.CTkLabel(self.scroll, text="", height=80)
        bot_pad.pack()
        self.labels.append(bot_pad)

    def update_position(self, seconds: float):
        if not self.is_synced or not self.lines:
            return

        # Find active line
        current_idx = -1
        for i, item in enumerate(self.lines):
            if item["time"] <= seconds:
                current_idx = i
            else:
                break

        if current_idx == self.active_index:
            return

        self.active_index = current_idx

        # Index in self.labels is offset by 1 because of top_pad
        for i, item in enumerate(self.lines):
            lbl_idx = i + 1
            if lbl_idx < len(self.labels):
                lbl = self.labels[lbl_idx]
                if i == current_idx:
                    lbl.configure(text_color=COLORS['text_primary'], font=FONTS['lyrics_active'])
                elif i < current_idx:
                    lbl.configure(text_color=COLORS['text_dim'], font=FONTS['lyrics'])
                else:
                    lbl.configure(text_color=COLORS['text_secondary'], font=FONTS['lyrics'])

        # Auto-scroll to center
        if 0 <= current_idx < len(self.lines):
            try:
                fraction = max(0.0, min(1.0, (current_idx - 1) / max(1, len(self.lines))))
                self.scroll._parent_canvas.yview_moveto(fraction)
            except Exception:
                pass

    def _seek_to(self, time_val: float):
        if time_val >= 0:
            self.app_window.audio_player.seek(time_val)

    def _copy_lyrics(self):
        if self._raw_text:
            self.clipboard_clear()
            self.clipboard_append(self._raw_text)
            self.copy_btn.configure(text="✓ Copied!")
            self.after(2000, lambda: self.copy_btn.configure(text="📋 Copy"))
