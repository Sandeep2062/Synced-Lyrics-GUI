"""Bottom status bar widget."""
import customtkinter as ctk
from typing import Any
from app.ui.theme import COLORS, FONTS

class StatusBar(ctk.CTkFrame):
    def __init__(self, master: Any, **kwargs):
        super().__init__(master, fg_color=COLORS['bg_secondary'], height=30, corner_radius=0, **kwargs)
        self.pack_propagate(False)
        
        # Left status
        self.status_lbl = ctk.CTkLabel(self, text="Ready", font=FONTS['small'], text_color=COLORS['text_secondary'])
        self.status_lbl.pack(side="left", padx=10)
        
        # Center playing info
        self.now_playing_lbl = ctk.CTkLabel(self, text="", font=FONTS['small'], text_color=COLORS['accent'])
        self.now_playing_lbl.pack(side="left", expand=True)
        
        # Right library stats
        self.stats_lbl = ctk.CTkLabel(self, text="0 synced | 0 plain | 0 missing", font=FONTS['small'], text_color=COLORS['text_muted'])
        self.stats_lbl.pack(side="right", padx=10)
        
    def set_status(self, text: str):
        self.status_lbl.configure(text=text)
        
    def set_now_playing(self, text: str):
        self.now_playing_lbl.configure(text=text)
        
    def set_stats(self, synced: int, plain: int, missing: int):
        self.stats_lbl.configure(text=f"{synced} synced | {plain} plain | {missing} missing")
