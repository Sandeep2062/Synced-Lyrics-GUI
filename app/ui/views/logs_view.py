"""Logs and reports view."""
import os
import time
from typing import Any
import tkinter.filedialog as filedialog
import customtkinter as ctk

from app.ui.theme import COLORS, FONTS
from app.core.lrc_utils import fmt_duration

class LogsView(ctk.CTkFrame):
    def __init__(self, master: Any, app_window: Any, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_primary'], **kwargs)
        self.app_window = app_window
        
        # Tabs Bar
        self.tabs_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], height=48, corner_radius=6)
        self.tabs_frame.pack(fill="x", padx=10, pady=(10, 4))
        self.tabs_frame.pack_propagate(False)
        
        self.tabs = {}
        self.current_tab = None
        
        self._add_tab("Not Found")
        self._add_tab("Suspicious")
        self._add_tab("History")
        self._add_tab("Rejected")
        
        # Actions on right
        self.export_btn = ctk.CTkButton(
            self.tabs_frame, 
            text="💾 Export Log", 
            width=100, 
            fg_color=COLORS['bg_hover'],
            hover_color=COLORS['border_light'],
            command=self._on_export_log
        )
        self.export_btn.pack(side="right", padx=(4, 10), pady=8)
        
        self.clear_btn = ctk.CTkButton(
            self.tabs_frame, 
            text="✕ Clear Cache", 
            width=100, 
            fg_color=COLORS['error'], 
            hover_color="#DC2626",
            command=self._on_clear_cache
        )
        self.clear_btn.pack(side="right", padx=4, pady=8)
        
        self.refresh_btn = ctk.CTkButton(
            self.tabs_frame,
            text="🔄 Refresh",
            width=80,
            fg_color=COLORS['bg_hover'],
            command=self._refresh_current_tab
        )
        self.refresh_btn.pack(side="right", padx=4, pady=8)

        # Log Content Display
        self.log_textbox = ctk.CTkTextbox(
            self, 
            fg_color=COLORS['bg_secondary'], 
            text_color=COLORS['text_secondary'], 
            font=FONTS['mono'],
            corner_radius=6
        )
        self.log_textbox.pack(fill="both", expand=True, padx=10, pady=(4, 10))
        
        self._switch_tab("Not Found")
        
    def _add_tab(self, name: str):
        btn = ctk.CTkButton(
            self.tabs_frame, 
            text=name, 
            fg_color="transparent", 
            text_color=COLORS['text_primary'], 
            hover_color=COLORS['bg_hover'],
            corner_radius=4,
            width=100,
            command=lambda: self._switch_tab(name)
        )
        btn.pack(side="left", padx=4, pady=8)
        self.tabs[name] = btn
        
    def _switch_tab(self, name: str):
        if self.current_tab and self.current_tab in self.tabs:
            self.tabs[self.current_tab].configure(fg_color="transparent")
        
        self.current_tab = name
        self.tabs[name].configure(fg_color=COLORS['accent'])
        self._refresh_current_tab()

    def _refresh_current_tab(self):
        name = self.current_tab
        lines = []
        db = self.app_window.db

        if name == "Not Found":
            # List tracks where lrc_status is missing
            missing_tracks = db.get_tracks_by_status("missing")
            lines.append(f"=== Tracks with No Lyrics Found ({len(missing_tracks)}) ===")
            lines.append("These tracks either had no lyrics available on any platform or were skipped.")
            lines.append("")
            for t in missing_tracks:
                artist = t.get('artist') or 'Unknown Artist'
                title = t.get('title') or os.path.basename(t.get('audio_path', ''))
                lines.append(f"• {artist} - {title}")
                lines.append(f"  Path: {t.get('audio_path')}")

        elif name == "Suspicious":
            susp_tracks = db.get_tracks_by_status("suspicious")
            lines.append(f"=== Suspicious Lyrics Flagged for Audit ({len(susp_tracks)}) ===")
            lines.append("These files have timestamp or title anomalies compared to the audio track.")
            lines.append("")
            for t in susp_tracks:
                artist = t.get('artist') or 'Unknown Artist'
                title = t.get('title') or os.path.basename(t.get('audio_path', ''))
                lines.append(f"⚠️ {artist} - {title}")
                lines.append(f"   Audio: {t.get('audio_path')}")
                lines.append(f"   Lyrics: {t.get('lrc_path')}")

        elif name == "History":
            all_tracks = db.get_all_tracks()
            lines.append(f"=== Download & Scan Activity History ({len(all_tracks)} tracks recorded) ===")
            lines.append("")
            for t in sorted(all_tracks, key=lambda x: x.get('last_checked') or 0, reverse=True)[:200]:
                checked_time = t.get('last_checked')
                time_str = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(checked_time)) if checked_time else "Never"
                status = t.get('lrc_status', 'missing').upper()
                source = t.get('lyrics_source') or 'Local'
                artist = t.get('artist') or 'Unknown'
                title = t.get('title') or os.path.basename(t.get('audio_path', ''))
                lines.append(f"[{time_str}] [{status}] {artist} - {title} (source: {source})")

        elif name == "Rejected":
            lines.append("=== Online Lyrics Discarded by Quality Auditor ===")
            lines.append("Lyrics found online that failed timestamp bounds or song title checks:")
            lines.append("")
            lines.append("(Logs are generated during 'Fix Suspicious' and full library audits)")

        text_content = "\n".join(lines) if len(lines) > 2 else f"--- {name} Logs ---\n\n(No entries recorded in database yet)"
        
        self.log_textbox.configure(state="normal")
        self.log_textbox.delete("1.0", "end")
        self.log_textbox.insert("end", text_content)
        self.log_textbox.configure(state="disabled")

    def _on_export_log(self):
        content = self.log_textbox.get("1.0", "end").strip()
        if not content:
            return
        dest = filedialog.asksaveasfilename(
            title="Export Log File", 
            defaultextension=".txt", 
            filetypes=[("Text File", "*.txt"), ("All Files", "*.*")]
        )
        if dest:
            try:
                with open(dest, "w", encoding="utf-8") as f:
                    f.write(content)
                self.app_window.status_bar.set_status(f"Log exported to {os.path.basename(dest)}")
            except Exception as e:
                self.app_window.status_bar.set_status(f"Export error: {e}")

    def _on_clear_cache(self):
        self.app_window.db.clear_cache()
        self.app_window.status_bar.set_status("Cleared lyrics search cache.")
        self._refresh_current_tab()
