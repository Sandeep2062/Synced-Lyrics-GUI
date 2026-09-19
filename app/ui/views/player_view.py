"""Music player view with synchronized lyrics display."""
import os
import customtkinter as ctk
from typing import Any, Optional

from app.ui.theme import COLORS, FONTS
from app.ui.widgets.lyrics_display import LyricsDisplay
from app.core import lrc_utils
from app.core.scanner import TrackInfo

class PlayerView(ctk.CTkFrame):
    def __init__(self, master: Any, app_window: Any, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_primary'], **kwargs)
        self.app_window = app_window
        self.current_track: Optional[TrackInfo] = None
        self._user_seeking = False
        self._duration = 0.0
        
        # Top Info Banner
        self.info_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], height=80, corner_radius=6)
        self.info_frame.pack(fill="x", padx=10, pady=(10, 4))
        self.info_frame.pack_propagate(False)
        
        self.art_lbl = ctk.CTkLabel(
            self.info_frame, 
            text="🎵", 
            font=("Segoe UI", 36), 
            fg_color=COLORS['bg_tertiary'], 
            width=64, 
            height=64, 
            corner_radius=6
        )
        self.art_lbl.pack(side="left", padx=10, pady=8)
        
        self.details_frame = ctk.CTkFrame(self.info_frame, fg_color="transparent")
        self.details_frame.pack(side="left", fill="both", expand=True, padx=10, pady=8)
        
        self.title_lbl = ctk.CTkLabel(
            self.details_frame, 
            text="No Track Playing", 
            font=FONTS['heading'], 
            text_color=COLORS['text_primary'], 
            anchor="w"
        )
        self.title_lbl.pack(fill="x")
        
        self.artist_lbl = ctk.CTkLabel(
            self.details_frame, 
            text="Select a song from the Library to play", 
            font=FONTS['body'], 
            text_color=COLORS['text_secondary'], 
            anchor="w"
        )
        self.artist_lbl.pack(fill="x")
        
        self.lrc_badge = ctk.CTkLabel(
            self.info_frame,
            text="",
            font=FONTS['small'],
            text_color=COLORS['accent'],
            width=100
        )
        self.lrc_badge.pack(side="right", padx=14)

        # Center Lyrics Display
        self.lyrics_display = LyricsDisplay(self, on_seek_request=self._on_lyrics_seek)
        self.lyrics_display.pack(fill="both", expand=True, padx=10, pady=4)
        
        # Bottom Controls Panel
        self.controls_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], height=86, corner_radius=6)
        self.controls_frame.pack(fill="x", padx=10, pady=(4, 10))
        self.controls_frame.pack_propagate(False)
        
        # Timeline Seek Bar
        self.timeline_frame = ctk.CTkFrame(self.controls_frame, fg_color="transparent")
        self.timeline_frame.pack(fill="x", padx=16, pady=(8, 0))
        
        self.curr_time = ctk.CTkLabel(self.timeline_frame, text="0:00", font=FONTS['mono'], text_color=COLORS['text_secondary'], width=45)
        self.curr_time.pack(side="left")
        
        self.seek_slider = ctk.CTkSlider(
            self.timeline_frame, 
            progress_color=COLORS['accent'], 
            button_color=COLORS['accent_dark'],
            command=self._on_seek
        )
        self.seek_slider.set(0)
        self.seek_slider.pack(side="left", fill="x", expand=True, padx=8)
        
        self.total_time = ctk.CTkLabel(self.timeline_frame, text="0:00", font=FONTS['mono'], text_color=COLORS['text_secondary'], width=45)
        self.total_time.pack(side="left")
        
        # Buttons Frame
        self.btns_frame = ctk.CTkFrame(self.controls_frame, fg_color="transparent")
        self.btns_frame.pack(fill="x", pady=(4, 8))
        
        self.prev_btn = ctk.CTkButton(
            self.btns_frame, 
            text="⏮", 
            width=36, 
            fg_color="transparent", 
            text_color=COLORS['text_primary'], 
            hover_color=COLORS['bg_hover'],
            command=self._on_prev_track
        )
        self.prev_btn.pack(side="left", padx=(180, 6), expand=True, anchor="e")
        
        self.play_btn = ctk.CTkButton(
            self.btns_frame, 
            text="▶", 
            width=46, 
            height=34,
            fg_color=COLORS['accent'], 
            hover_color=COLORS['accent_hover'], 
            text_color=COLORS['text_primary'], 
            font=FONTS['subheading'],
            command=self._toggle_playback
        )
        self.play_btn.pack(side="left", padx=8)
        
        self.next_btn = ctk.CTkButton(
            self.btns_frame, 
            text="⏭", 
            width=36, 
            fg_color="transparent", 
            text_color=COLORS['text_primary'], 
            hover_color=COLORS['bg_hover'],
            command=self._on_next_track
        )
        self.next_btn.pack(side="left", padx=(6, 180), expand=True, anchor="w")
        
        # Volume
        self.vol_lbl = ctk.CTkLabel(self.btns_frame, text="🔊", text_color=COLORS['text_secondary'])
        self.vol_lbl.pack(side="left", padx=(0, 4))
        
        self.vol_slider = ctk.CTkSlider(
            self.btns_frame, 
            width=90, 
            progress_color=COLORS['accent'],
            command=self._on_volume_change
        )
        self.vol_slider.set(self.app_window.config.volume)
        self.vol_slider.pack(side="left", padx=(0, 16))

        # Start playback monitor loop
        self._update_playback_loop()

    def play_track(self, track: Any):
        self.current_track = track
        
        # Extract fields
        audio_path = getattr(track, 'audio_path', None) or track.get('audio_path', '')
        title = getattr(track, 'title', None) or track.get('title', '')
        artist = getattr(track, 'artist', None) or track.get('artist', '')
        album = getattr(track, 'album', None) or track.get('album', '')
        lrc_path = getattr(track, 'lrc_path', None) or track.get('lrc_path', '')
        duration = getattr(track, 'duration', None) or track.get('duration', 0.0) or 0.0
        
        if not title and audio_path:
            title = os.path.splitext(os.path.basename(audio_path))[0]
            
        self.title_lbl.configure(text=title or "Unknown Title")
        self.artist_lbl.configure(text=f"{artist or 'Unknown Artist'} • {album or 'Unknown Album'}")
        self._duration = float(duration) if duration else 0.0
        
        if self._duration > 0:
            self.total_time.configure(text=lrc_utils.fmt_duration(self._duration))
        else:
            self.total_time.configure(text="--:--")

        # Load audio into player
        if audio_path and os.path.exists(audio_path):
            success = self.app_window.audio_player.load(audio_path)
            if success:
                self.app_window.audio_player.play()
                self.play_btn.configure(text="⏸")
                self.app_window.status_bar.set_now_playing(f"▶ {artist} - {title}")
            else:
                self.app_window.status_bar.set_status(f"Could not play audio format: {os.path.basename(audio_path)}")

        # Load lyrics
        lrc_text = ""
        if lrc_path and os.path.exists(lrc_path):
            lrc_text = lrc_utils.read_text(lrc_path)
        elif hasattr(track, 'lrc_content') and track.lrc_content:
            lrc_text = track.lrc_content

        self.lyrics_display.set_lyrics(lrc_text)
        
        if lrc_text and lrc_utils.is_synced(lrc_text):
            self.lrc_badge.configure(text="✅ Synced Lyrics", text_color=COLORS['success'])
        elif lrc_text and lrc_text.strip():
            self.lrc_badge.configure(text="📝 Plain Lyrics", text_color=COLORS['warning'])
        else:
            self.lrc_badge.configure(text="❌ No Lyrics", text_color=COLORS['error'])

    def _toggle_playback(self):
        player = self.app_window.audio_player
        if player.is_playing:
            player.pause()
            self.play_btn.configure(text="▶")
            self.app_window.status_bar.set_status("Paused")
        else:
            player.resume()
            self.play_btn.configure(text="⏸")
            self.app_window.status_bar.set_status("Playing")

    def _on_seek(self, value: float):
        if self._duration > 0:
            target_pos = value * self._duration
            self.app_window.audio_player.seek(target_pos)
            self.lyrics_display.update_position(target_pos)

    def _on_lyrics_seek(self, time_val: float):
        if self._duration > 0 and time_val <= self._duration:
            self.app_window.audio_player.seek(time_val)
            self.seek_slider.set(time_val / self._duration)
            self.curr_time.configure(text=lrc_utils.fmt_duration(time_val))

    def _on_volume_change(self, value: float):
        self.app_window.audio_player.set_volume(value)
        self.app_window.config.volume = value

    def _on_prev_track(self):
        # Previous track logic
        pass

    def _on_next_track(self):
        # Next track logic
        pass

    def _update_playback_loop(self):
        player = self.app_window.audio_player
        if player.is_playing:
            pos = player.position
            self.curr_time.configure(text=lrc_utils.fmt_duration(pos))
            if self._duration > 0:
                fraction = max(0.0, min(1.0, pos / self._duration))
                self.seek_slider.set(fraction)
            self.lyrics_display.update_position(pos)
            
        self.after(100, self._update_playback_loop)
