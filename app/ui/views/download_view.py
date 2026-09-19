"""Batch download progress view."""
import time
import customtkinter as ctk
from typing import Any, List
from app.ui.theme import COLORS, FONTS
from app.ui.widgets.progress_panel import ProgressPanel
from app.core.scanner import TrackInfo
from app.core.fetcher import FetchResult

class DownloadView(ctk.CTkFrame):
    def __init__(self, master: Any, app_window: Any, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_primary'], **kwargs)
        self.app_window = app_window
        self._start_time = 0.0
        self._synced_count = 0
        self._plain_count = 0
        self._kept_count = 0
        self._miss_count = 0
        self._error_count = 0
        
        # Top Progress Section
        self.progress_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], corner_radius=6)
        self.progress_frame.pack(fill="x", padx=10, pady=(10, 6))
        
        self.title_lbl = ctk.CTkLabel(
            self.progress_frame, 
            text="Batch Download Progress", 
            font=FONTS['heading'], 
            text_color=COLORS['text_primary']
        )
        self.title_lbl.pack(pady=(12, 4))
        
        self.progress_bar = ctk.CTkProgressBar(self.progress_frame, progress_color=COLORS['accent'], height=12)
        self.progress_bar.set(0)
        self.progress_bar.pack(fill="x", padx=24, pady=8)
        
        self.eta_lbl = ctk.CTkLabel(
            self.progress_frame, 
            text="Idle • Ready to download", 
            font=FONTS['body'], 
            text_color=COLORS['text_secondary']
        )
        self.eta_lbl.pack(pady=(0, 10))
        
        # Progress Panel (Current song, Platforms, Logs, Stats)
        self.progress_panel = ProgressPanel(self)
        self.progress_panel.pack(fill="both", expand=True, padx=10, pady=4)
        
        # Controls Frame
        self.controls_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], height=48, corner_radius=6)
        self.controls_frame.pack(fill="x", padx=10, pady=(4, 10))
        self.controls_frame.pack_propagate(False)
        
        self.pause_btn = ctk.CTkButton(
            self.controls_frame, 
            text="⏸ Pause", 
            width=90, 
            fg_color=COLORS['warning'], 
            hover_color="#D97706",
            command=self._on_pause_clicked
        )
        self.pause_btn.pack(side="left", padx=10, pady=8)
        
        self.resume_btn = ctk.CTkButton(
            self.controls_frame, 
            text="▶ Resume", 
            width=90, 
            fg_color=COLORS['success'], 
            hover_color="#16A34A",
            command=self._on_resume_clicked
        )
        self.resume_btn.pack(side="left", padx=6, pady=8)
        
        self.stop_btn = ctk.CTkButton(
            self.controls_frame, 
            text="⏹ Stop", 
            width=90, 
            fg_color=COLORS['error'], 
            hover_color="#DC2626",
            command=self._on_stop_clicked
        )
        self.stop_btn.pack(side="right", padx=10, pady=8)

    def start_download(self, tracks: List[TrackInfo], mode: str):
        self._start_time = time.time()
        self._synced_count = 0
        self._plain_count = 0
        self._kept_count = 0
        self._miss_count = 0
        self._error_count = 0
        
        self.progress_bar.set(0)
        self.progress_panel.clear()
        self.progress_panel.log(f"Starting batch download in '{mode}' mode...")
        self.eta_lbl.configure(text=f"Mode: {mode.upper()} • Initializing...")
        
        self.pause_btn.configure(state="normal")
        self.resume_btn.configure(state="normal")
        self.stop_btn.configure(state="normal")

        self.app_window.fetcher.start_batch(
            tracks=tracks,
            mode=mode,
            on_track_start=self._on_track_start,
            on_track_result=self._on_track_result,
            on_platform_status=self._on_platform_status,
            on_rate_limit=self._on_rate_limit,
            on_complete=self._on_complete
        )

    def _on_track_start(self, idx: int, total: int, track: TrackInfo):
        def update_ui():
            pct = idx / max(1, total)
            self.progress_bar.set(pct)
            
            # Estimate ETA
            elapsed = time.time() - self._start_time
            if idx > 0 and elapsed > 1.0:
                rate = idx / elapsed
                remaining_sec = int((total - idx) / rate)
                mins, secs = divmod(remaining_sec, 60)
                eta_str = f"ETA: {mins:02d}m {secs:02d}s"
            else:
                eta_str = "ETA: calculating..."
                
            self.eta_lbl.configure(text=f"{int(pct * 100)}% ({idx + 1}/{total}) • {eta_str}")
            
            song_title = f"{track.artist or 'Unknown'} - {track.title or 'Unknown'}"
            self.progress_panel.set_current_song(song_title)

        self.after(0, update_ui)

    def _on_platform_status(self, track: TrackInfo, platform: str, status: str):
        self.after(0, lambda: self.progress_panel.set_platform_status(platform, status))

    def _on_track_result(self, idx: int, total: int, result: FetchResult):
        def update_ui():
            if result.status == 'synced':
                self._synced_count += 1
                icon = "✅ SYNCED"
            elif result.status == 'plain':
                self._plain_count += 1
                icon = "📝 PLAIN"
            elif result.status == 'kept':
                self._kept_count += 1
                icon = "🛡️ KEPT"
            elif result.status == 'error':
                self._error_count += 1
                icon = "💥 ERROR"
            else:
                self._miss_count += 1
                icon = "❌ NOT FOUND"
                
            log_msg = f"[{icon}] {result.query}"
            if result.source:
                log_msg += f" (via {result.source})"
            if result.error:
                log_msg += f" - {result.error}"
                
            self.progress_panel.log(log_msg)
            self.progress_panel.update_stats(
                self._synced_count, self._plain_count, self._kept_count, self._miss_count, self._error_count
            )
            
        self.after(0, update_ui)

    def _on_rate_limit(self, platform: str, retry_seconds: int):
        self.after(0, lambda: self.progress_panel.set_rate_limit_warning(
            f"{platform} hit rate limits. Pausing requests or backing off for {retry_seconds}s..."
        ))

    def _on_complete(self, summary: dict):
        def finish_ui():
            self.progress_bar.set(1.0)
            elapsed = time.time() - self._start_time
            mins, secs = divmod(int(elapsed), 60)
            
            self.eta_lbl.configure(text=f"Complete in {mins:02d}m {secs:02d}s • Synced: {summary['synced']}, Plain: {summary['plain']}, Missed: {summary['none']}")
            self.progress_panel.log("=" * 50)
            self.progress_panel.log(f"Batch completed in {mins:02d}m {secs:02d}s!")
            self.progress_panel.log(f"Synced: {summary['synced']} | Plain: {summary['plain']} | Kept: {summary['kept']} | Not Found: {summary['none']} | Errors: {summary['error']}")
            self.progress_panel.set_rate_limit_warning("")
            self.app_window.status_bar.set_status("Batch download finished.")
            
        self.after(0, finish_ui)

    def _on_pause_clicked(self):
        self.app_window.fetcher.pause()
        self.progress_panel.log("⏸ Batch paused by user.")
        self.app_window.status_bar.set_status("Batch paused.")

    def _on_resume_clicked(self):
        self.app_window.fetcher.resume()
        self.progress_panel.log("▶ Batch resumed by user.")
        self.app_window.status_bar.set_status("Batch running...")

    def _on_stop_clicked(self):
        self.app_window.fetcher.cancel()
        self.progress_panel.log("⏹ Batch stopped by user.")
        self.app_window.status_bar.set_status("Batch cancelled.")
