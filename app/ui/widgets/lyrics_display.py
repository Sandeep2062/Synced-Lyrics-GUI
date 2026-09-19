"""Synced lyrics viewer widget."""
import re
import customtkinter as ctk
from typing import Any, List, Dict
from app.ui.theme import COLORS, FONTS
from app.core.lrc_utils import STAMP_RE

class LyricsDisplay(ctk.CTkScrollableFrame):
    def __init__(self, master: Any, on_seek_request=None, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_primary'], **kwargs)
        self.on_seek_request = on_seek_request
        self.lines: List[Dict[str, Any]] = []
        self.labels: List[ctk.CTkLabel] = []
        self.active_index = -1
        self.is_synced = False
        
        # Placeholder
        self.placeholder = ctk.CTkLabel(
            self,
            text="No lyrics loaded",
            font=FONTS['lyrics'],
            text_color=COLORS['text_muted']
        )
        self.placeholder.pack(expand=True, pady=100)

    def set_lyrics(self, lrc_text: str):
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
                self,
                text="No lyrics loaded",
                font=FONTS['lyrics'],
                text_color=COLORS['text_muted']
            )
            self.placeholder.pack(expand=True, pady=100)
            return

        # Parse lyrics lines
        raw_lines = lrc_text.strip().splitlines()
        synced_entries = []
        plain_entries = []

        for line in raw_lines:
            line_str = line.strip()
            if not line_str:
                continue
            
            # Match timestamp tags
            matches = STAMP_RE.findall(line_str)
            if matches:
                # Remove timestamps from the text content
                text = STAMP_RE.sub('', line_str).strip()
                # A line can have multiple timestamp tags
                for m, s, f in matches:
                    t = int(m) * 60 + int(s)
                    if f:
                        t += int(f) / (10 ** len(f))
                    synced_entries.append({"time": t, "text": text})
            else:
                # Skip metadata tags like [ti:], [ar:], [al:], etc.
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

        if not self.lines:
            self.placeholder = ctk.CTkLabel(
                self,
                text="Instrumental or empty lyrics",
                font=FONTS['lyrics'],
                text_color=COLORS['text_muted']
            )
            self.placeholder.pack(expand=True, pady=100)
            return

        # Render labels
        for idx, line_data in enumerate(self.lines):
            lbl = ctk.CTkLabel(
                self,
                text=line_data['text'] if line_data['text'] else "♪",
                font=FONTS['lyrics'],
                text_color=COLORS['text_secondary'],
                wraplength=650,
                justify="center",
                cursor="hand2" if self.is_synced else "arrow"
            )
            lbl.pack(pady=6, anchor="center")
            
            if self.is_synced:
                t = line_data['time']
                lbl.bind("<Button-1>", lambda e, time_val=t: self._on_line_click(time_val))
                
            self.labels.append(lbl)

    def _on_line_click(self, time_val: float):
        if time_val >= 0 and self.on_seek_request:
            self.on_seek_request(time_val)

    def update_position(self, seconds: float):
        if not self.is_synced or not self.lines or not self.labels:
            return

        # Find the line that should be active for current seconds
        current_idx = -1
        for i, item in enumerate(self.lines):
            if item["time"] <= seconds:
                current_idx = i
            else:
                break

        if current_idx == self.active_index:
            return

        self.active_index = current_idx

        # Update styling
        for i, lbl in enumerate(self.labels):
            if i == current_idx:
                lbl.configure(text_color=COLORS['accent'], font=FONTS['lyrics_active'])
            elif i < current_idx:
                lbl.configure(text_color=COLORS['text_muted'], font=FONTS['lyrics'])
            else:
                lbl.configure(text_color=COLORS['text_secondary'], font=FONTS['lyrics'])

        # Scroll to center the active label
        if 0 <= current_idx < len(self.labels):
            active_lbl = self.labels[current_idx]
            try:
                total_labels = len(self.labels)
                fraction = max(0.0, min(1.0, (current_idx - 2) / max(1, total_labels)))
                self._parent_canvas.yview_moveto(fraction)
            except Exception:
                pass
