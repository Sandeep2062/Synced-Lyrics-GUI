"""LRCGET persistent bottom now-playing player bar."""
import os
import customtkinter as ctk
from typing import Any, Optional

from app.ui.theme import COLORS, FONTS
from app.core import lrc_utils
from app.core.art_cache import get_thumbnail

class NowPlayingBar(ctk.CTkFrame):
    """
    Persistent bottom bar matching LRCGET:
    - Full-width pink scrub slider with 00:00 / 00:00
    - Left: 40x40 thumbnail, bold title, artist
    - Center: ↺10, circular pink play/pause, ↻10
    - Right: Lyrics toggle, speed dropdown, volume
    """
    def __init__(self, master: Any, app_window: Any, on_toggle_lyrics: Any, **kwargs):
        super().__init__(
            master, 
            fg_color=COLORS['bg_toolbar'], 
            height=72, 
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
        
        # 1. Top Timeline Scrubber (Full Width)
        self.timeline_frame = ctk.CTkFrame(self, fg_color="transparent", height=16)
        self.timeline_frame.pack(fill="x", padx=16, pady=(4, 0))
        
        self.curr_time_lbl = ctk.CTkLabel(
            self.timeline_frame, 
            text="00:00", 
            font=FONTS['mono'], 
            text_color=COLORS['text_muted'],
            width=40
        )
        self.curr_time_lbl.pack(side="left")
        
        self.seek_slider = ctk.CTkSlider(
            self.timeline_frame, 
            progress_color=COLORS['accent'], 
            button_color=COLORS['accent'],
            button_hover_color=COLORS['accent_hover'],
            fg_color=COLORS['slider_rail'],
            height=10,
            command=self._on_seek
        )
        self.seek_slider.set(0.0)
        self.seek_slider.pack(side="left", fill="x", expand=True, padx=8)
        
        self.total_time_lbl = ctk.CTkLabel(
            self.timeline_frame, 
            text="00:00", 
            font=FONTS['mono'], 
            text_color=COLORS['text_muted'],
            width=40
        )
        self.total_time_lbl.pack(side="right")
        
        # 2. Controls & Track Details Row
        self.content_row = ctk.CTkFrame(self, fg_color="transparent")
        self.content_row.pack(fill="both", expand=True, padx=16, pady=(0, 4))
        
        # Left: Thumbnail + Title + Artist (Clickable to open lyrics drawer)
        self.info_frame = ctk.CTkFrame(self.content_row, fg_color="transparent", cursor="hand2")
        self.info_frame.pack(side="left", fill="y", padx=(0, 20))
        self.info_frame.bind("<Button-1>", lambda e: self.on_toggle_lyrics())
        
        self.thumb_lbl = ctk.CTkLabel(self.info_frame, text="", width=38, height=38)
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
        self.title_lbl.pack(fill="x")
        self.title_lbl.bind("<Button-1>", lambda e: self.on_toggle_lyrics())
        
        self.artist_lbl = ctk.CTkLabel(
            self.text_col, 
            text="Click a track to play", 
            font=FONTS['small'], 
            text_color=COLORS['text_secondary'],
            anchor="w"
        )
        self.artist_lbl.pack(fill="x")
        self.artist_lbl.bind("<Button-1>", lambda e: self.on_toggle_lyrics())

        # Center: Rewind 10s, Large Round Play/Pause, Forward 10s
        self.center_frame = ctk.CTkFrame(self.content_row, fg_color="transparent")
        self.center_frame.pack(side="left", expand=True)
        
        self.rewind_btn = ctk.CTkButton(
            self.center_frame, 
            text="↺ 10", 
            width=36, 
            height=30,
            fg_color="transparent", 
            text_color=COLORS['text_primary'], 
            hover_color=COLORS['bg_button'],
            font=FONTS['small_bold'],
            command=self._rewind_10
        )
        self.rewind_btn.pack(side="left", padx=6)
        
        self.play_btn = ctk.CTkButton(
            self.center_frame, 
            text="▶", 
            width=40, 
            height=40,
            corner_radius=20,
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
            width=36, 
            height=30,
            fg_color="transparent", 
            text_color=COLORS['text_primary'], 
            hover_color=COLORS['bg_button'],
            font=FONTS['small_bold'],
            command=self._forward_10
        )
        self.forward_btn.pack(side="left", padx=6)

        # Right: Lyrics Button, Speed Dropdown, Volume
        self.right_frame = ctk.CTkFrame(self.content_row, fg_color="transparent")
        self.right_frame.pack(side="right")
        
        self.lyrics_drawer_btn = ctk.CTkButton(
            self.right_frame, 
            text="📝 Lyrics", 
            width=70, 
            height=28,
            fg_color=COLORS['bg_button'], 
            hover_color=COLORS['bg_button_hover'],
            text_color=COLORS['text_primary'],
            font=FONTS['small_bold'],
            command=self.on_toggle_lyrics
        )
        self.lyrics_drawer_btn.pack(side="left", padx=(0, 10))
        
        self.speed_cb = ctk.CTkComboBox(
            self.right_frame, 
            values=["0.75x", "1x", "1.25x", "1.5x"], 
            width=68, 
            height=26,
            fg_color=COLORS['bg_button'], 
            button_color=COLORS['border'],
            font=FONTS['small']
        )
        self.speed_cb.set("1x")
        self.speed_cb.pack(side="left", padx=(0, 10))
        
        self.vol_lbl = ctk.CTkLabel(self.right_frame, text="🔊", font=FONTS['small'], text_color=COLORS['text_muted'])
        self.vol_lbl.pack(side="left", padx=(0, 4))
        
        self.vol_slider = ctk.CTkSlider(
            self.right_frame, 
            width=80, 
            height=10,
            progress_color=COLORS['accent'],
            button_color=COLORS['accent'],
            fg_color=COLORS['slider_rail'],
            command=self._on_volume
        )
        self.vol_slider.set(self.app_window.config.volume)
        self.vol_slider.pack(side="left")

        # Start poll loop
        self._update_loop()

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
            
        # Update thumbnail
        thumb = get_thumbnail(audio_path, size=(38, 38))
        self.thumb_lbl.configure(image=thumb)
        
        # Load audio into player
        if audio_path and os.path.exists(audio_path):
            success = self.app_window.audio_player.load(audio_path)
            if success:
                self.app_window.audio_player.play()
                self.play_btn.configure(text="⏸")
            else:
                self.app_window.status_bar.set_status("Playback error: format not supported")

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

    def _on_seek(self, value: float):
        if self._duration > 0:
            pos = value * self._duration
            self.app_window.audio_player.seek(pos)
            self.curr_time_lbl.configure(text=lrc_utils.fmt_duration(pos))

    def _on_volume(self, value: float):
        self.app_window.audio_player.set_volume(value)
        self.app_window.config.volume = value

    def _update_loop(self):
        player = self.app_window.audio_player
        if player.is_playing and self._duration > 0:
            pos = player.position
            self.curr_time_lbl.configure(text=lrc_utils.fmt_duration(pos))
            fraction = max(0.0, min(1.0, pos / self._duration))
            self.seek_slider.set(fraction)
            # Update slide-up lyrics viewer if open
            if hasattr(self.app_window, 'lyrics_drawer'):
                self.app_window.lyrics_drawer.update_position(pos)
                
        self.after(100, self._update_loop)
