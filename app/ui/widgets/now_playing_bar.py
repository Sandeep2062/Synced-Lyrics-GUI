"""LRCGET persistent bottom now-playing player bar with visual waveform seekbar."""
import os
import math
import random
import tkinter as tk
import customtkinter as ctk
from typing import Any, Optional, Callable

from app.ui.theme import COLORS, FONTS
from app.core import lrc_utils
from app.core.art_cache import _get_placeholder, load_thumbnail_async


class WaveformScrubber(ctk.CTkFrame):
    """
    A visual audio waveform seekbar.
    Renders 90 vertical audio bars that fill with pink accent as playback progresses.
    Clicking or dragging on the waveform seeks audio directly.
    """
    BARS_COUNT = 90

    def __init__(self, master: Any, on_seek: Callable[[float], None], **kwargs):
        super().__init__(master, fg_color="transparent", height=18, **kwargs)
        self.on_seek = on_seek
        self.progress: float = 0.0
        self._is_dragging: bool = False

        # Pre-generate pleasant waveform envelope
        random.seed(42)
        self._bar_heights = [
            max(0.18, min(0.95, 0.5 + 0.35 * math.sin(i * 0.22) + 0.15 * math.cos(i * 0.48) + random.uniform(-0.08, 0.08)))
            for i in range(self.BARS_COUNT)
        ]

        self.canvas = tk.Canvas(self, bg=COLORS['bg_toolbar'], highlightthickness=0, height=18)
        self.canvas.pack(fill="both", expand=True)

        self.canvas.bind("<Configure>", lambda e: self.draw())
        self.canvas.bind("<Button-1>", self._on_press)
        self.canvas.bind("<B1-Motion>", self._on_drag)
        self.canvas.bind("<ButtonRelease-1>", self._on_release)

    def set_progress(self, progress: float):
        if not self._is_dragging:
            self.progress = max(0.0, min(1.0, progress))
            self.draw()

    def draw(self):
        self.canvas.delete("all")
        w = self.canvas.winfo_width()
        h = self.canvas.winfo_height()
        if w < 20 or h < 5:
            return

        step = w / self.BARS_COUNT
        bar_width = max(2.0, step - 2.0)
        mid_y = h / 2.0
        active_threshold_x = self.progress * w

        for i in range(self.BARS_COUNT):
            x = i * step + 1.0
            bar_h = self._bar_heights[i] * (h - 4.0)
            y1 = mid_y - (bar_h / 2.0)
            y2 = mid_y + (bar_h / 2.0)

            if x <= active_threshold_x:
                color = COLORS['accent'] # Pink filled
            else:
                color = COLORS['slider_rail'] # Subtle rail

            self.canvas.create_rectangle(x, y1, x + bar_width, y2, fill=color, outline="", width=0)

        # Scrubber cursor knob
        knob_x = max(3.0, min(w - 3.0, active_threshold_x))
        self.canvas.create_oval(
            knob_x - 4.0, mid_y - 7.0,
            knob_x + 4.0, mid_y + 7.0,
            fill=COLORS['accent_cyan'],
            outline="#FFFFFF",
            width=1
        )

    def _on_press(self, event):
        self._is_dragging = True
        self._handle_event(event)

    def _on_drag(self, event):
        self._handle_event(event)

    def _on_release(self, event):
        self._handle_event(event)
        self._is_dragging = False

    def _handle_event(self, event):
        w = self.canvas.winfo_width()
        if w > 0:
            frac = max(0.0, min(1.0, event.x / w))
            self.progress = frac
            self.draw()
            if self.on_seek:
                self.on_seek(frac)


class NowPlayingBar(ctk.CTkFrame):
    """
    Persistent bottom player bar matching LRCGET:
    - Top: Interactive visual waveform seekbar with 00:00 / 00:00
    - Left: 44x44 rounded cover thumbnail, bold title, clean artist • album
    - Center: ↺10, large circular pink play/pause, 10↻
    - Right: Lyrics toggle pill, speed dropdown, volume
    """
    def __init__(self, master: Any, app_window: Any, on_toggle_lyrics: Any, **kwargs):
        super().__init__(
            master, 
            fg_color=COLORS['bg_toolbar'], 
            height=86, 
            corner_radius=0, 
            border_width=1,
            border_color=COLORS['border'],
            **kwargs
        )
        self.pack_propagate(False)
        self.app_window = app_window
        self.on_toggle_lyrics = on_toggle_lyrics
        self.current_track: Optional[dict] = None
        self._duration: float = 0.0
        self._is_seeking: bool = False
        
        # 1. Top Waveform Scrubber Row
        self.timeline_frame = ctk.CTkFrame(self, fg_color="transparent", height=20)
        self.timeline_frame.pack(fill="x", padx=16, pady=(4, 2))
        self.timeline_frame.pack_propagate(False)
        
        self.curr_time_lbl = ctk.CTkLabel(
            self.timeline_frame, 
            text="00:00", 
            font=FONTS['mono'], 
            text_color=COLORS['text_muted'],
            width=42,
            anchor="w"
        )
        self.curr_time_lbl.pack(side="left")
        
        self.waveform = WaveformScrubber(
            self.timeline_frame,
            on_seek=self._on_seek
        )
        self.waveform.pack(side="left", fill="both", expand=True, padx=8)
        
        self.total_time_lbl = ctk.CTkLabel(
            self.timeline_frame, 
            text="00:00", 
            font=FONTS['mono'], 
            text_color=COLORS['text_muted'],
            width=42,
            anchor="e"
        )
        self.total_time_lbl.pack(side="right")
        
        # 2. Controls & Track Details Row (56px tall)
        self.content_row = ctk.CTkFrame(self, fg_color="transparent", height=56)
        self.content_row.pack(fill="x", padx=16, pady=(2, 4))
        self.content_row.pack_propagate(False)
        
        # Left: Thumbnail + Title + Artist (Clickable to open lyrics drawer)
        self.info_frame = ctk.CTkFrame(self.content_row, fg_color="transparent", cursor="hand2")
        self.info_frame.pack(side="left", fill="y", padx=(0, 20))
        self.info_frame.bind("<Button-1>", lambda e: self.on_toggle_lyrics())
        
        # 44x44 cover thumbnail, initialized with dark placeholder immediately
        self.thumb_lbl = ctk.CTkLabel(
            self.info_frame, 
            text="", 
            image=_get_placeholder((44, 44)), 
            width=44, 
            height=44
        )
        self.thumb_lbl.pack(side="left", padx=(0, 10))
        self.thumb_lbl.bind("<Button-1>", lambda e: self.on_toggle_lyrics())
        
        self.text_col = ctk.CTkFrame(self.info_frame, fg_color="transparent")
        self.text_col.pack(side="left", fill="y", pady=2)
        self.text_col.bind("<Button-1>", lambda e: self.on_toggle_lyrics())
        
        self.title_lbl = ctk.CTkLabel(
            self.text_col, 
            text="No track selected", 
            font=FONTS['body_bold'], 
            text_color=COLORS['text_primary'],
            anchor="w"
        )
        self.title_lbl.pack(fill="x", anchor="w")
        self.title_lbl.bind("<Button-1>", lambda e: self.on_toggle_lyrics())
        
        self.artist_lbl = ctk.CTkLabel(
            self.text_col, 
            text="Click any track in your library to play", 
            font=FONTS['small'], 
            text_color=COLORS['text_secondary'],
            anchor="w"
        )
        self.artist_lbl.pack(fill="x", anchor="w", pady=(1, 0))
        self.artist_lbl.bind("<Button-1>", lambda e: self.on_toggle_lyrics())

        # Center: Rewind 10s, Large Circular Pink Play/Pause, Forward 10s
        self.center_frame = ctk.CTkFrame(self.content_row, fg_color="transparent")
        self.center_frame.pack(side="left", expand=True)
        
        self.rewind_btn = ctk.CTkButton(
            self.center_frame, 
            text="↺ 10", 
            width=38, 
            height=32,
            fg_color="transparent", 
            text_color=COLORS['text_primary'], 
            hover_color=COLORS['bg_button'],
            font=FONTS['small_bold'],
            command=self._rewind_10
        )
        self.rewind_btn.pack(side="left", padx=8)
        
        self.play_btn = ctk.CTkButton(
            self.center_frame, 
            text="▶", 
            width=42, 
            height=42,
            corner_radius=21,
            fg_color=COLORS['accent'], 
            hover_color=COLORS['accent_hover'], 
            text_color="#FFFFFF",
            font=FONTS['subheading'],
            command=self._toggle_playback
        )
        self.play_btn.pack(side="left", padx=10)
        
        self.forward_btn = ctk.CTkButton(
            self.center_frame, 
            text="10 ↻", 
            width=38, 
            height=32,
            fg_color="transparent", 
            text_color=COLORS['text_primary'], 
            hover_color=COLORS['bg_button'],
            font=FONTS['small_bold'],
            command=self._forward_10
        )
        self.forward_btn.pack(side="left", padx=8)

        # Right: Lyrics Button, Speed Dropdown, Volume
        self.right_frame = ctk.CTkFrame(self.content_row, fg_color="transparent")
        self.right_frame.pack(side="right", padx=(20, 0))
        
        self.lyrics_btn = ctk.CTkButton(
            self.right_frame, 
            text="🎤 Lyrics", 
            width=76, 
            height=30,
            fg_color=COLORS['bg_button'], 
            hover_color=COLORS['bg_button_hover'],
            text_color=COLORS['text_primary'],
            font=FONTS['small_bold'],
            command=self.on_toggle_lyrics
        )
        self.lyrics_btn.pack(side="left", padx=6)
        
        # Speed Dropdown
        self.speed_var = ctk.StringVar(value="1x")
        self.speed_menu = ctk.CTkOptionMenu(
            self.right_frame, 
            values=["0.5x", "0.75x", "1x", "1.25x", "1.5x", "2x"],
            variable=self.speed_var,
            width=54,
            height=28,
            font=FONTS['small'],
            fg_color=COLORS['bg_button'],
            button_color=COLORS['border'],
            command=self._on_speed_changed
        )
        self.speed_menu.pack(side="left", padx=6)
        
        # Volume Icon
        self.vol_lbl = ctk.CTkLabel(self.right_frame, text="🔊", font=FONTS['small'], text_color=COLORS['text_muted'])
        self.vol_lbl.pack(side="left", padx=(6, 2))
        
        # Volume Slider
        self.vol_slider = ctk.CTkSlider(
            self.right_frame, 
            from_=0.0, 
            to=1.0, 
            number_of_steps=100, 
            width=80, 
            height=8,
            progress_color=COLORS['accent'],
            button_color=COLORS['accent'],
            button_hover_color=COLORS['accent_hover'],
            fg_color=COLORS['slider_rail'],
            command=self._on_volume
        )
        self.vol_slider.pack(side="left", padx=(2, 6))
        self.vol_slider.set(getattr(self.app_window.config, 'volume', 0.7))

        # Periodic update loop (every 250ms)
        self.after(250, self._update_loop)

    def _on_speed_changed(self, choice: str):
        speed_val = float(choice.replace("x", ""))
        self.app_window.audio_player.set_speed(speed_val)

    def set_track(self, track: Any):
        self.current_track = track
        
        audio_path = getattr(track, 'audio_path', None) or (track.get('audio_path', '') if isinstance(track, dict) else '')
        title = getattr(track, 'title', None) or (track.get('title', '') if isinstance(track, dict) else '')
        artist = getattr(track, 'artist', None) or (track.get('artist', '') if isinstance(track, dict) else '')
        album = getattr(track, 'album', None) or (track.get('album', '') if isinstance(track, dict) else '')
        dur = getattr(track, 'duration', None) or (track.get('duration', 0.0) if isinstance(track, dict) else 0.0)
        
        if not title and audio_path:
            title = os.path.splitext(os.path.basename(audio_path))[0]
            
        self.title_lbl.configure(text=title or "Unknown Title")
        self.artist_lbl.configure(text=f"{artist or 'Unknown Artist'} • {album or 'Unknown Album'}")
        
        self._duration = float(dur) if dur else 0.0
        if self._duration > 0:
            self.total_time_lbl.configure(text=lrc_utils.fmt_duration(self._duration))
        else:
            self.total_time_lbl.configure(text="00:00")
            
        # Update thumbnail asynchronously
        load_thumbnail_async(audio_path, size=(44, 44), target_widget=self.thumb_lbl)
        
        # Load audio into player
        if audio_path and os.path.exists(audio_path):
            success = self.app_window.audio_player.load(audio_path)
            if success:
                self.app_window.audio_player.play()
                self.play_btn.configure(text="⏸")
            else:
                self.app_window.set_status("Playback error: format not supported")

    def _toggle_playback(self):
        player = self.app_window.audio_player
        if player.is_playing:
            player.pause()
            self.play_btn.configure(text="▶")
        else:
            player.resume()
            self.play_btn.configure(text="⏸")

    def _rewind_10(self):
        player = self.app_window.audio_player
        new_pos = max(0.0, player.position - 10.0)
        player.seek(new_pos)

    def _forward_10(self):
        player = self.app_window.audio_player
        if self._duration > 0:
            new_pos = min(self._duration, player.position + 10.0)
            player.seek(new_pos)

    def _on_seek(self, fraction: float):
        if self._duration > 0:
            pos = fraction * self._duration
            self.app_window.audio_player.seek(pos)
            self.curr_time_lbl.configure(text=lrc_utils.fmt_duration(pos))

    def _on_volume(self, value: float):
        self.app_window.audio_player.set_volume(value)
        self.app_window.config.volume = value

    def _update_loop(self):
        try:
            if not self.winfo_exists():
                return
        except Exception:
            return

        try:
            player = self.app_window.audio_player
            if player.is_playing and self._duration > 0:
                pos = player.position
                self.curr_time_lbl.configure(text=lrc_utils.fmt_duration(pos))
                fraction = max(0.0, min(1.0, pos / self._duration))
                self.waveform.set_progress(fraction)
                # Update slide-up lyrics viewer if open
                if hasattr(self.app_window, 'lyrics_drawer'):
                    self.app_window.lyrics_drawer.update_position(pos)
                    
            self.after(250, self._update_loop)
        except Exception:
            pass

