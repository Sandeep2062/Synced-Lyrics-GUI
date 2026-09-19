"""Live download progress widget."""
import customtkinter as ctk
from typing import Any
from app.ui.theme import COLORS, FONTS, STATUS_ICONS, STATUS_COLORS

class ProgressPanel(ctk.CTkFrame):
    def __init__(self, master: Any, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_secondary'], corner_radius=6, **kwargs)
        
        # Current Song
        self.current_song_lbl = ctk.CTkLabel(
            self, 
            text="Currently Processing: Idle", 
            font=FONTS['subheading'], 
            text_color=COLORS['text_primary'],
            anchor="w"
        )
        self.current_song_lbl.pack(fill="x", pady=(12, 4), padx=14)
        
        # Platform Status indicators
        self.platform_frame = ctk.CTkFrame(self, fg_color="transparent")
        self.platform_frame.pack(fill="x", padx=14, pady=(2, 8))
        
        self.platforms = {}
        self.platform_names = ["LRCLib", "Musixmatch", "NetEase", "Megalobiz", "Genius"]
        for p in self.platform_names:
            pill = ctk.CTkFrame(self.platform_frame, fg_color=COLORS['bg_tertiary'], corner_radius=4)
            pill.pack(side="left", padx=(0, 8), pady=2)
            lbl = ctk.CTkLabel(pill, text=f"⏭ {p}", text_color=COLORS['text_muted'], font=FONTS['small'])
            lbl.pack(padx=8, pady=4)
            self.platforms[p] = lbl
            
        # Rate Limit Warning Banner
        self.rate_limit_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_tertiary'], corner_radius=4)
        self.rate_limit_lbl = ctk.CTkLabel(
            self.rate_limit_frame, 
            text="", 
            text_color=COLORS['warning'], 
            font=FONTS['small']
        )
        self.rate_limit_lbl.pack(padx=10, pady=4)
        # Not packed initially, only when rate limit occurs
        
        # Log Box
        self.log_lbl = ctk.CTkLabel(self, text="Activity Log", font=FONTS['small'], text_color=COLORS['text_muted'], anchor="w")
        self.log_lbl.pack(fill="x", padx=14, pady=(6, 2))
        
        self.log_box = ctk.CTkTextbox(
            self, 
            fg_color=COLORS['bg_tertiary'], 
            text_color=COLORS['text_secondary'], 
            font=FONTS['mono'], 
            height=180,
            corner_radius=4
        )
        self.log_box.pack(fill="both", expand=True, padx=14, pady=(0, 8))
        self.log_box.configure(state="disabled")
        
        # Overall Stats Bar
        self.stats_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_tertiary'], height=32, corner_radius=4)
        self.stats_frame.pack(fill="x", padx=14, pady=(0, 12))
        self.stats_frame.pack_propagate(False)
        
        self.stats_lbl = ctk.CTkLabel(
            self.stats_frame, 
            text="Synced: 0 | Plain: 0 | Kept: 0 | Not Found: 0 | Errors: 0", 
            font=FONTS['small'],
            text_color=COLORS['text_secondary']
        )
        self.stats_lbl.pack(expand=True)

    def set_current_song(self, song_title: str):
        self.current_song_lbl.configure(text=f"🎵 Processing: {song_title}")
        # Reset platform statuses to pending
        for p, lbl in self.platforms.items():
            lbl.configure(text=f"⏭ {p}", text_color=COLORS['text_muted'])

    def set_platform_status(self, platform: str, status: str):
        if platform in self.platforms:
            icon = STATUS_ICONS.get(status, '⏭')
            color = STATUS_COLORS.get(status, COLORS['text_secondary'])
            self.platforms[platform].configure(text=f"{icon} {platform}", text_color=color)

    def set_rate_limit_warning(self, message: str):
        if message:
            self.rate_limit_lbl.configure(text=f"⚠️ {message}")
            if not self.rate_limit_frame.winfo_ismapped():
                self.rate_limit_frame.pack(fill="x", padx=14, pady=(2, 6), before=self.log_lbl)
        else:
            if self.rate_limit_frame.winfo_ismapped():
                self.rate_limit_frame.pack_forget()

    def update_stats(self, synced: int, plain: int, kept: int, not_found: int, errors: int):
        self.stats_lbl.configure(
            text=f"✅ Synced: {synced} | 📝 Plain: {plain} | 🛡️ Kept: {kept} | ❌ Not Found: {not_found} | 💥 Errors: {errors}"
        )

    def log(self, message: str):
        self.log_box.configure(state="normal")
        self.log_box.insert("end", message + "\n")
        self.log_box.see("end")
        self.log_box.configure(state="disabled")

    def clear(self):
        self.log_box.configure(state="normal")
        self.log_box.delete("1.0", "end")
        self.log_box.configure(state="disabled")
        self.set_rate_limit_warning("")
        self.current_song_lbl.configure(text="Currently Processing: Idle")
        for p, lbl in self.platforms.items():
            lbl.configure(text=f"⏭ {p}", text_color=COLORS['text_muted'])
