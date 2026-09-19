"""Logs and reports view with structured cards, status metrics, and search."""
import os
import time
from typing import Any, List, Dict
import tkinter.filedialog as filedialog
import customtkinter as ctk

from app.ui.theme import COLORS, FONTS, get_status_colors

class LogsView(ctk.CTkFrame):
    def __init__(self, master: Any, app_window: Any, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_primary'], **kwargs)
        self.app_window = app_window
        self.current_tab = "Missing"
        self.view_mode = "cards"  # "cards" or "raw"
        self.cached_entries: List[Dict[str, Any]] = []
        
        # 1. Summary Metrics Bar
        self.metrics_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_toolbar'], height=60, corner_radius=6)
        self.metrics_frame.pack(fill="x", padx=10, pady=(10, 6))
        self.metrics_frame.pack_propagate(False)
        
        self.metric_missing = self._create_metric_pill(self.metrics_frame, "Missing Lyrics", "0", COLORS['pill_missing_bg'], COLORS['pill_missing_fg'])
        self.metric_suspicious = self._create_metric_pill(self.metrics_frame, "Suspicious Flagged", "0", COLORS['pill_suspicious_bg'], COLORS['pill_suspicious_fg'])
        self.metric_synced = self._create_metric_pill(self.metrics_frame, "Synced Lyrics", "0", COLORS['pill_synced_bg'], COLORS['pill_synced_fg'])
        self.metric_total = self._create_metric_pill(self.metrics_frame, "Total Scanned", "0", COLORS['bg_button'], COLORS['text_primary'])

        # 2. Controls & Tabs Bar
        self.toolbar = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], height=44, corner_radius=6)
        self.toolbar.pack(fill="x", padx=10, pady=(0, 6))
        self.toolbar.pack_propagate(False)
        
        # Subtabs
        self.tabs = {}
        for tab_name in ["Missing", "Suspicious", "History", "Rejected"]:
            btn = ctk.CTkButton(
                self.toolbar, 
                text=tab_name, 
                fg_color="transparent", 
                text_color=COLORS['text_muted'], 
                hover_color=COLORS['bg_hover'],
                corner_radius=4,
                width=85,
                height=28,
                font=FONTS['small_bold'],
                command=lambda name=tab_name: self._switch_tab(name)
            )
            btn.pack(side="left", padx=3, pady=8)
            self.tabs[tab_name] = btn

        # Actions on right
        self.export_btn = ctk.CTkButton(
            self.toolbar, 
            text="💾 Export", 
            width=70, 
            height=28,
            font=FONTS['small_bold'],
            fg_color=COLORS['bg_button'],
            hover_color=COLORS['bg_button_hover'],
            command=self._on_export_log
        )
        self.export_btn.pack(side="right", padx=(2, 8), pady=8)
        
        self.clear_btn = ctk.CTkButton(
            self.toolbar, 
            text="✕ Clear Cache", 
            width=90, 
            height=28,
            font=FONTS['small_bold'],
            fg_color=COLORS['error'], 
            hover_color="#DC2626",
            command=self._on_clear_cache
        )
        self.clear_btn.pack(side="right", padx=2, pady=8)
        
        self.toggle_mode_btn = ctk.CTkButton(
            self.toolbar,
            text="📝 Raw View",
            width=80,
            height=28,
            font=FONTS['small_bold'],
            fg_color=COLORS['bg_button'],
            hover_color=COLORS['bg_button_hover'],
            command=self._toggle_view_mode
        )
        self.toggle_mode_btn.pack(side="right", padx=2, pady=8)

        self.refresh_btn = ctk.CTkButton(
            self.toolbar,
            text="🔄",
            width=32,
            height=28,
            font=FONTS['small_bold'],
            fg_color=COLORS['bg_button'],
            hover_color=COLORS['bg_button_hover'],
            command=self._refresh_current_tab
        )
        self.refresh_btn.pack(side="right", padx=2, pady=8)

        # Search bar
        self.search_var = ctk.StringVar()
        self.search_var.trace_add("write", lambda *args: self._filter_entries())
        self.search_entry = ctk.CTkEntry(
            self.toolbar,
            placeholder_text="🔍 Filter log entries...",
            width=200,
            height=28,
            font=FONTS['small'],
            fg_color=COLORS['bg_input'],
            border_color=COLORS['border'],
            textvariable=self.search_var
        )
        self.search_entry.pack(side="right", padx=6, pady=8)

        # 3. Content Containers
        # Card List Display
        self.cards_scroll = ctk.CTkScrollableFrame(self, fg_color=COLORS['bg_primary'])
        self.cards_scroll.pack(fill="both", expand=True, padx=10, pady=(0, 10))

        # Raw Text Display (hidden by default)
        self.log_textbox = ctk.CTkTextbox(
            self, 
            fg_color=COLORS['bg_secondary'], 
            text_color=COLORS['text_secondary'], 
            font=FONTS['mono'],
            corner_radius=6
        )

        self._switch_tab("Missing")

    def _create_metric_pill(self, parent, label: str, value: str, bg_color: str, fg_color: str):
        pill = ctk.CTkFrame(parent, fg_color=bg_color, corner_radius=6, height=40)
        pill.pack(side="left", padx=8, pady=10)
        pill.pack_propagate(False)
        
        lbl = ctk.CTkLabel(pill, text=f"{label}: {value}", font=FONTS['small_bold'], text_color=fg_color)
        lbl.pack(padx=12, pady=8)
        return lbl

    def _toggle_view_mode(self):
        if self.view_mode == "cards":
            self.view_mode = "raw"
            self.cards_scroll.pack_forget()
            self.log_textbox.pack(fill="both", expand=True, padx=10, pady=(0, 10))
            self.toggle_mode_btn.configure(text="🗂️ Card View")
        else:
            self.view_mode = "cards"
            self.log_textbox.pack_forget()
            self.cards_scroll.pack(fill="both", expand=True, padx=10, pady=(0, 10))
            self.toggle_mode_btn.configure(text="📝 Raw View")
        self._refresh_current_tab()

    def _switch_tab(self, name: str):
        for tname, btn in self.tabs.items():
            if tname == name:
                btn.configure(fg_color=COLORS['accent'], text_color="#FFFFFF")
            else:
                btn.configure(fg_color="transparent", text_color=COLORS['text_muted'])
        
        self.current_tab = name
        self._refresh_current_tab()

    def _refresh_metrics(self):
        db = self.app_window.db
        stats = db.get_stats()
        missing = stats.get('missing', 0)
        suspicious = stats.get('suspicious', 0)
        synced = stats.get('synced', 0)
        total = sum(stats.values()) if stats else 0

        self.metric_missing.configure(text=f"Missing: {missing}")
        self.metric_suspicious.configure(text=f"Suspicious: {suspicious}")
        self.metric_synced.configure(text=f"Synced: {synced}")
        self.metric_total.configure(text=f"Total Tracks: {total}")

    def _refresh_current_tab(self):
        self._refresh_metrics()
        name = self.current_tab
        db = self.app_window.db
        
        if name == "Missing":
            self.cached_entries = db.get_tracks_by_status("missing")
        elif name == "Suspicious":
            self.cached_entries = db.get_tracks_by_status("suspicious")
        elif name == "History":
            all_tracks = db.get_all_tracks()
            self.cached_entries = sorted(all_tracks, key=lambda x: x.get('last_checked') or 0, reverse=True)[:250]
        elif name == "Rejected":
            self.cached_entries = []

        self._filter_entries()

    def _filter_entries(self):
        query = self.search_var.get().strip().lower()
        if query:
            filtered = [
                e for e in self.cached_entries
                if query in (e.get('title') or '').lower()
                or query in (e.get('artist') or '').lower()
                or query in (e.get('audio_path') or '').lower()
            ]
        else:
            filtered = self.cached_entries

        # Update Card View
        for w in self.cards_scroll.winfo_children():
            w.destroy()

        if not filtered:
            empty_lbl = ctk.CTkLabel(
                self.cards_scroll,
                text=f"No {self.current_tab.lower()} log entries found.",
                font=FONTS['body'],
                text_color=COLORS['text_muted']
            )
            empty_lbl.pack(pady=40)
        else:
            # Show up to 100 entries in card view for fast performance
            for item in filtered[:100]:
                self._render_log_card(item)

        # Update Raw View
        raw_lines = [f"=== {self.current_tab} Logs ({len(filtered)} entries) ===", ""]
        for item in filtered:
            artist = item.get('artist') or 'Unknown'
            title = item.get('title') or os.path.basename(item.get('audio_path', ''))
            path = item.get('audio_path') or ''
            status = item.get('lrc_status') or 'missing'
            raw_lines.append(f"[{status.upper()}] {artist} - {title}")
            raw_lines.append(f"  Path: {path}")
            if item.get('lrc_path'):
                raw_lines.append(f"  Lyrics: {item.get('lrc_path')}")
            raw_lines.append("")

        text_content = "\n".join(raw_lines)
        self.log_textbox.configure(state="normal")
        self.log_textbox.delete("1.0", "end")
        self.log_textbox.insert("end", text_content)
        self.log_textbox.configure(state="disabled")

    def _render_log_card(self, item: Dict[str, Any]):
        card = ctk.CTkFrame(self.cards_scroll, fg_color=COLORS['bg_secondary'], height=52, corner_radius=6)
        card.pack(fill="x", pady=2)
        card.pack_propagate(False)

        # Left: Status badge
        status = item.get('lrc_status', 'missing')
        bg_col, fg_col = get_status_colors(status)
        badge = ctk.CTkFrame(card, fg_color=bg_col, corner_radius=10, height=22)
        badge.pack(side="left", padx=(10, 8), pady=14)
        badge_lbl = ctk.CTkLabel(badge, text=status.upper(), font=FONTS['small_bold'], text_color=fg_col)
        badge_lbl.pack(padx=8, pady=1)

        # Center: Title + Subtitle
        col = ctk.CTkFrame(card, fg_color="transparent")
        col.pack(side="left", fill="both", expand=True, pady=6)
        
        artist = item.get('artist') or 'Unknown Artist'
        title = item.get('title') or os.path.basename(item.get('audio_path', ''))
        path = item.get('audio_path', '')
        
        t_lbl = ctk.CTkLabel(col, text=f"{artist} - {title}", font=FONTS['body_bold'], text_color=COLORS['text_primary'], anchor="w")
        t_lbl.pack(fill="x")
        
        p_lbl = ctk.CTkLabel(col, text=path, font=FONTS['small'], text_color=COLORS['text_muted'], anchor="w")
        p_lbl.pack(fill="x")

        # Right: Quick Download button
        btn = ctk.CTkButton(
            card,
            text="⬇ Fetch",
            width=65,
            height=26,
            font=FONTS['small_bold'],
            fg_color=COLORS['bg_button'],
            hover_color=COLORS['bg_button_hover'],
            command=lambda it=item: self.app_window.download_single_track(it)
        )
        btn.pack(side="right", padx=10, pady=12)

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
