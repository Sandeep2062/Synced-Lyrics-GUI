"""Settings view with custom API configuration, engines manager, directories, and auto-updater."""
import os
import threading
import tkinter.filedialog as filedialog
import customtkinter as ctk
from typing import Any

from app.ui.theme import COLORS, FONTS
from app.constants import APP_VERSION
from app.updater import check_for_updates, download_update, apply_update
from app.engine_manager import EngineManager

class SettingsView(ctk.CTkScrollableFrame):
    def __init__(self, master: Any, app_window: Any, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_primary'], **kwargs)
        self.app_window = app_window
        self.engine_mgr = EngineManager()
        
        # 1. Music Directories Section
        self.create_section_header("📁 Music Directories")
        self.dirs_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], corner_radius=6)
        self.dirs_frame.pack(fill="x", padx=10, pady=(0, 16))
        
        self.dirs_list_frame = ctk.CTkFrame(self.dirs_frame, fg_color="transparent")
        self.dirs_list_frame.pack(fill="x", padx=10, pady=10)
        
        self.add_dir_btn = ctk.CTkButton(
            self.dirs_frame, 
            text="+ Add Music Folder", 
            fg_color=COLORS['accent'], 
            hover_color=COLORS['accent_hover'],
            command=self._on_add_directory
        )
        self.add_dir_btn.pack(pady=(0, 12), padx=10, anchor="w")
        
        # 2. API Keys & Custom Instances
        self.create_section_header("🔑 API Keys & Custom Platforms (Default: Built-in)")
        self.keys_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], corner_radius=6)
        self.keys_frame.pack(fill="x", padx=10, pady=(0, 16))
        
        # A. LRCLib Instance
        lrclib_curr = self.app_window.config.get_api_key("lrclib_instance") or "https://lrclib.net"
        self.lrclib_entry = self.create_platform_config_entry(
            parent=self.keys_frame,
            title="LRCLib Database Instance",
            field_label="Instance URL:",
            current_value=lrclib_curr,
            placeholder="https://lrclib.net",
            default_value="https://lrclib.net",
            is_secret=False,
            on_save=lambda val: self._save_key("lrclib_instance", val),
            help_text=(
                "• Default: Public community instance (https://lrclib.net) — completely free with ~60 req/min.\n"
                "• Custom: You can point this to your own self-hosted LRCLib instance (Docker / lrcget server) if desired."
            )
        )

        # B. Musixmatch Key
        mm_key = self.app_window.config.get_api_key("musixmatch") or ""
        self.mm_entry = self.create_platform_config_entry(
            parent=self.keys_frame,
            title="Musixmatch Provider",
            field_label="API Key:",
            current_value=mm_key,
            placeholder="Leave empty to use Built-in (Recommended)",
            default_value="",
            is_secret=True,
            on_save=lambda val: self._save_key("musixmatch", val),
            help_text=(
                "• Built-in Mode (Recommended): Uses internal token rotation to retrieve 100% FULL synchronized lyrics.\n"
                "• Custom API Key: Register at developer.musixmatch.com to create an app. Free API key allows 2,000 calls/day (10 req/min), "
                "but Musixmatch limits free API responses to 30-40% lyrics previews.\n"
                "💡 Tip: Keep blank to enjoy full synced lyrics through the built-in engine!"
            )
        )
        
        # C. Genius Token
        genius_key = self.app_window.config.get_api_key("genius") or ""
        self.genius_entry = self.create_platform_config_entry(
            parent=self.keys_frame,
            title="Genius Fallback Provider (Plain-Text)",
            field_label="Client Token:",
            current_value=genius_key,
            placeholder="Leave empty to use Built-in Web Engine",
            default_value="",
            is_secret=True,
            on_save=lambda val: self._save_key("genius", val),
            help_text=(
                "• Built-in Mode: Automatically scrapes plain-text lyrics from Genius when no synced lyrics are found anywhere.\n"
                "• Custom Client Token: Go to genius.com/api-clients ➔ 'New API Client' ➔ Generate 'Client Access Token' to authenticate requests directly."
            )
        )

        # 3. Engines & Dependencies Section (FFmpeg & Syncedlyrics)
        self.create_section_header("📦 External Engines & Dependencies")
        self.engines_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], corner_radius=6)
        self.engines_frame.pack(fill="x", padx=10, pady=(0, 16))
        
        # Syncedlyrics Engine Row
        sl_row = ctk.CTkFrame(self.engines_frame, fg_color="transparent")
        sl_row.pack(fill="x", padx=14, pady=8)
        
        curr_sl_v = self.engine_mgr.get_syncedlyrics_version()
        self.sl_status_lbl = ctk.CTkLabel(
            sl_row, 
            text=f"Syncedlyrics Engine: v{curr_sl_v} (Ready)", 
            font=FONTS['body'],
            text_color=COLORS['success'],
            anchor="w"
        )
        self.sl_status_lbl.pack(side="left")
        
        self.sl_update_btn = ctk.CTkButton(
            sl_row, 
            text="Check Engine Update", 
            width=150, 
            fg_color=COLORS['bg_hover'],
            command=self._on_check_syncedlyrics_update
        )
        self.sl_update_btn.pack(side="right")
        
        # FFmpeg Audio Engine Row
        ffmpeg_row = ctk.CTkFrame(self.engines_frame, fg_color="transparent")
        ffmpeg_row.pack(fill="x", padx=14, pady=8)
        
        self.ffmpeg_status_lbl = ctk.CTkLabel(
            ffmpeg_row, 
            text="Checking FFmpeg installation...", 
            font=FONTS['body'],
            anchor="w"
        )
        self.ffmpeg_status_lbl.pack(side="left")
        
        self.ffmpeg_btn = ctk.CTkButton(
            ffmpeg_row, 
            text="Download FFmpeg", 
            width=150, 
            fg_color=COLORS['bg_hover'],
            command=self._on_install_ffmpeg
        )
        self.ffmpeg_btn.pack(side="right")

        # 4. Performance & Limits Section
        self.create_section_header("⚡ Performance & Rate Limiting")
        self.perf_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], corner_radius=6)
        self.perf_frame.pack(fill="x", padx=10, pady=(0, 16))
        
        int_frame = ctk.CTkFrame(self.perf_frame, fg_color="transparent")
        int_frame.pack(fill="x", padx=14, pady=8)
        
        ctk.CTkLabel(int_frame, text="Request Interval (politeness delay):", font=FONTS['body'], width=240, anchor="w").pack(side="left")
        self.interval_val_lbl = ctk.CTkLabel(int_frame, text=f"{self.app_window.config.request_interval:.1f}s", font=FONTS['mono'], width=50)
        self.interval_val_lbl.pack(side="right")
        
        self.interval_slider = ctk.CTkSlider(
            int_frame, 
            from_=0.1, 
            to=3.0, 
            number_of_steps=29, 
            progress_color=COLORS['accent'],
            command=self._on_interval_changed
        )
        self.interval_slider.set(self.app_window.config.request_interval)
        self.interval_slider.pack(side="right", fill="x", expand=True, padx=10)

        retry_frame = ctk.CTkFrame(self.perf_frame, fg_color="transparent")
        retry_frame.pack(fill="x", padx=14, pady=8)
        
        ctk.CTkLabel(retry_frame, text="Skip recently checked tracks for:", font=FONTS['body'], width=240, anchor="w").pack(side="left")
        self.retry_cb = ctk.CTkComboBox(
            retry_frame, 
            values=["7 days", "14 days", "30 days", "Always re-check"],
            command=self._on_retry_days_changed,
            fg_color=COLORS['bg_input'],
            button_color=COLORS['border']
        )
        self.retry_cb.set(f"{self.app_window.config.retry_days} days")
        self.retry_cb.pack(side="left", padx=10)

        # 5. Software Update Section
        self.create_section_header("🔄 Software Updates")
        self.update_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], corner_radius=6)
        self.update_frame.pack(fill="x", padx=10, pady=(0, 16))
        
        self.version_lbl = ctk.CTkLabel(
            self.update_frame, 
            text=f"Current Version: v{APP_VERSION}", 
            font=FONTS['body']
        )
        self.version_lbl.pack(side="left", padx=14, pady=12)
        
        self.update_status_lbl = ctk.CTkLabel(
            self.update_frame,
            text="",
            font=FONTS['small'],
            text_color=COLORS['text_secondary']
        )
        self.update_status_lbl.pack(side="left", padx=10, pady=12)

        self.update_btn = ctk.CTkButton(
            self.update_frame, 
            text="Check App Updates", 
            width=150,
            fg_color=COLORS['accent'], 
            hover_color=COLORS['accent_hover'],
            command=self._on_check_updates
        )
        self.update_btn.pack(side="right", padx=14, pady=12)
        
        # 6. About Section
        self.create_section_header("ℹ️ About")
        self.about_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_secondary'], corner_radius=6)
        self.about_frame.pack(fill="x", padx=10, pady=(0, 20))
        
        about_text = (
            f"Synced Lyrics GUI v{APP_VERSION}\n"
            "An autonomous multi-provider lyrics manager and music player for local libraries.\n"
            "Supports LRCLib, Musixmatch, NetEase, Megalobiz, and Genius."
        )
        ctk.CTkLabel(self.about_frame, text=about_text, font=FONTS['body'], justify="left").pack(padx=14, pady=(12, 6), anchor="w")
        
        # Refresh UI elements
        self._refresh_directories_list()
        self._refresh_ffmpeg_status()

    def create_section_header(self, text: str):
        lbl = ctk.CTkLabel(self, text=text, font=FONTS['subheading'], text_color=COLORS['accent'])
        lbl.pack(anchor="w", padx=10, pady=(12, 4))
        
    def create_platform_config_entry(self, parent, title, field_label, current_value, placeholder, default_value, is_secret, on_save, help_text):
        container = ctk.CTkFrame(parent, fg_color=COLORS['bg_tertiary'], corner_radius=6)
        container.pack(fill="x", padx=12, pady=6)
        
        ctk.CTkLabel(container, text=title, font=FONTS['subheading'], text_color=COLORS['text_primary'], anchor="w").pack(fill="x", padx=10, pady=(8, 2))
        
        input_row = ctk.CTkFrame(container, fg_color="transparent")
        input_row.pack(fill="x", padx=10, pady=4)
        
        ctk.CTkLabel(input_row, text=field_label, width=110, anchor="w", font=FONTS['body']).pack(side="left")
        
        entry = ctk.CTkEntry(
            input_row, 
            width=340, 
            fg_color=COLORS['bg_input'], 
            border_color=COLORS['border'], 
            placeholder_text=placeholder,
            show="*" if is_secret else ""
        )
        if current_value and current_value != default_value:
            entry.insert(0, current_value)
        entry.pack(side="left", padx=8)
        
        save_btn = ctk.CTkButton(
            input_row, 
            text="Save", 
            width=60, 
            fg_color=COLORS['accent'],
            hover_color=COLORS['accent_hover'],
            command=lambda: on_save(entry.get().strip())
        )
        save_btn.pack(side="left", padx=4)
        
        reset_btn = ctk.CTkButton(
            input_row, 
            text="Reset", 
            width=60, 
            fg_color="transparent",
            text_color=COLORS['text_muted'],
            hover_color=COLORS['bg_hover'],
            command=lambda: (entry.delete(0, 'end'), on_save(default_value))
        )
        reset_btn.pack(side="left", padx=2)
        
        guide = ctk.CTkLabel(
            container, 
            text=help_text, 
            text_color=COLORS['text_muted'], 
            font=FONTS['small'], 
            wraplength=700, 
            justify="left"
        )
        guide.pack(fill="x", padx=10, pady=(4, 8), anchor="w")
        return entry

    def _refresh_directories_list(self):
        for widget in self.dirs_list_frame.winfo_children():
            widget.destroy()
            
        dirs = self.app_window.config.directories
        if not dirs:
            lbl = ctk.CTkLabel(
                self.dirs_list_frame, 
                text="No music directories configured yet.", 
                font=FONTS['body'], 
                text_color=COLORS['text_muted']
            )
            lbl.pack(anchor="w", pady=4)
            return

        for d in dirs:
            row = ctk.CTkFrame(self.dirs_list_frame, fg_color=COLORS['bg_tertiary'], corner_radius=4)
            row.pack(fill="x", pady=2)
            
            ctk.CTkLabel(row, text=f"📂  {d}", font=FONTS['body'], text_color=COLORS['text_primary']).pack(side="left", padx=10, pady=6)
            
            rm_btn = ctk.CTkButton(
                row, 
                text="✕ Remove", 
                width=80, 
                fg_color="transparent", 
                text_color=COLORS['error'],
                hover_color=COLORS['bg_hover'],
                command=lambda path=d: self._on_remove_directory(path)
            )
            rm_btn.pack(side="right", padx=8, pady=4)

    def _on_add_directory(self):
        chosen = filedialog.askdirectory(title="Add Music Directory")
        if chosen:
            self.app_window.config.add_directory(chosen)
            self.app_window.db.add_directory(chosen)
            self._refresh_directories_list()

    def _on_remove_directory(self, path: str):
        self.app_window.config.remove_directory(path)
        self.app_window.db.remove_directory(path)
        self._refresh_directories_list()

    def _save_key(self, provider: str, key_val: str):
        self.app_window.config.set_api_key(provider, key_val)
        self.app_window.provider_manager.configure_api_keys(self.app_window.config.api_keys)
        self.app_window.status_bar.set_status(f"Updated {provider} configuration.")

    def _on_interval_changed(self, val: float):
        self.interval_val_lbl.configure(text=f"{val:.1f}s")
        self.app_window.config.request_interval = round(val, 2)
        self.app_window.config.save()

    def _on_retry_days_changed(self, choice: str):
        if "7" in choice:
            days = 7
        elif "14" in choice:
            days = 14
        elif "30" in choice:
            days = 30
        else:
            days = 0
        self.app_window.config.retry_days = days
        self.app_window.config.save()

    def _on_check_syncedlyrics_update(self):
        self.sl_status_lbl.configure(text="Checking PyPI for syncedlyrics updates...", text_color=COLORS['text_secondary'])
        self.sl_update_btn.configure(state="disabled")

        def check():
            has_update, curr, latest = self.engine_mgr.check_syncedlyrics_update()
            def ui_update():
                self.sl_update_btn.configure(state="normal")
                if has_update:
                    self.sl_status_lbl.configure(text=f"New version available: v{latest} (installed: v{curr})", text_color=COLORS['warning'])
                    self.sl_update_btn.configure(text="Update Engine", command=self._on_perform_syncedlyrics_update)
                else:
                    self.sl_status_lbl.configure(text=f"Syncedlyrics is up-to-date: v{curr}", text_color=COLORS['success'])
            self.after(0, ui_update)

        threading.Thread(target=check, daemon=True).start()

    def _on_perform_syncedlyrics_update(self):
        self.sl_status_lbl.configure(text="Updating syncedlyrics via pip...", text_color=COLORS['text_secondary'])
        self.sl_update_btn.configure(state="disabled")

        def worker():
            success, msg = self.engine_mgr.update_syncedlyrics()
            def ui_update():
                self.sl_update_btn.configure(state="normal", text="Check Engine Update", command=self._on_check_syncedlyrics_update)
                color = COLORS['success'] if success else COLORS['warning']
                self.sl_status_lbl.configure(text=msg, text_color=color)
                self.app_window.status_bar.set_status(msg)
            self.after(0, ui_update)

        threading.Thread(target=worker, daemon=True).start()

    def _refresh_ffmpeg_status(self):
        installed = self.app_window.ffmpeg_manager.is_installed()
        if installed:
            self.ffmpeg_status_lbl.configure(text=f"✅ FFmpeg available: {self.app_window.ffmpeg_manager.get_path()}", text_color=COLORS['success'])
            self.ffmpeg_btn.configure(text="Re-download", fg_color=COLORS['bg_hover'])
        else:
            self.ffmpeg_status_lbl.configure(text="⚠️ FFmpeg not downloaded in app folder", text_color=COLORS['warning'])
            self.ffmpeg_btn.configure(text="Download Now", fg_color=COLORS['accent'])

    def _on_install_ffmpeg(self):
        self.ffmpeg_status_lbl.configure(text="Downloading FFmpeg from GitHub releases...", text_color=COLORS['text_secondary'])
        self.ffmpeg_btn.configure(state="disabled")

        def dl():
            success = self.app_window.ffmpeg_manager.download(lambda p: None)
            self.after(0, lambda: self._on_ffmpeg_done(success))

        threading.Thread(target=dl, daemon=True).start()

    def _on_ffmpeg_done(self, success: bool):
        self.ffmpeg_btn.configure(state="normal")
        if success:
            self._refresh_ffmpeg_status()
            self.app_window.status_bar.set_status("FFmpeg successfully downloaded and configured!")
        else:
            self.ffmpeg_status_lbl.configure(text="❌ Failed to download FFmpeg automatically. Check internet connection.", text_color=COLORS['error'])

    def _on_check_updates(self):
        self.update_btn.configure(state="disabled")
        self.update_status_lbl.configure(text="Checking for latest release...")

        def worker():
            has_update, latest_tag, download_url = check_for_updates()
            self.after(0, lambda: self._on_update_result(has_update, latest_tag, download_url))

        threading.Thread(target=worker, daemon=True).start()

    def _on_update_result(self, has_update: bool, latest_tag: str, download_url: str):
        self.update_btn.configure(state="normal")
        if has_update:
            self.update_status_lbl.configure(text=f"New version available: v{latest_tag}!", text_color=COLORS['success'])
            self.update_btn.configure(
                text=f"Update to v{latest_tag}",
                command=lambda: self._download_and_apply(download_url)
            )
        else:
            self.update_status_lbl.configure(text=f"You are running the latest version (v{APP_VERSION}).", text_color=COLORS['text_secondary'])

    def _download_and_apply(self, download_url: str):
        self.update_status_lbl.configure(text="Downloading update...")
        self.update_btn.configure(state="disabled")

        def dl_worker():
            try:
                temp_exe = download_update(download_url, on_progress=lambda p: None)
                self.after(0, lambda: apply_update(temp_exe))
            except Exception as e:
                self.after(0, lambda: self.update_status_lbl.configure(text=f"Update failed: {e}", text_color=COLORS['error']))

        threading.Thread(target=dl_worker, daemon=True).start()
