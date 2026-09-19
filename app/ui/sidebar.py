"""Navigation sidebar component."""
import customtkinter as ctk
from typing import Callable, Dict
from app.ui.theme import COLORS, FONTS

class Sidebar(ctk.CTkFrame):
    def __init__(self, master: any, on_navigate: Callable[[str], None], **kwargs):
        super().__init__(master, fg_color=COLORS['bg_primary'], width=200, corner_radius=0, **kwargs)
        
        self.on_navigate = on_navigate
        self.nav_items: Dict[str, ctk.CTkButton] = {}
        
        # Title
        self.title_label = ctk.CTkLabel(
            self, 
            text="Synced Lyrics", 
            font=FONTS['heading'], 
            text_color=COLORS['accent']
        )
        self.title_label.pack(pady=(20, 30), padx=20, anchor="w")
        
        # Navigation Buttons
        self._add_nav_item("Library", "📚 library")
        self._add_nav_item("Download", "⬇️ download")
        self._add_nav_item("Player", "🎵 player")
        self._add_nav_item("Logs", "📋 logs")
        
        # Spacer
        self.spacer = ctk.CTkFrame(self, fg_color="transparent")
        self.spacer.pack(expand=True, fill="both")
        
        # Settings at bottom
        self._add_nav_item("Settings", "⚙️ settings", pady=(0, 20))
        
        # Set default active
        self.set_active("library")

    def _add_nav_item(self, text: str, view_id: str, pady=(0, 5)):
        # Extract icon and pure id
        parts = view_id.split(' ', 1)
        icon = parts[0]
        actual_id = parts[1] if len(parts) > 1 else view_id.lower()
        
        btn = ctk.CTkButton(
            self,
            text=f" {icon}  {text}",
            font=FONTS['subheading'],
            fg_color="transparent",
            text_color=COLORS['text_primary'],
            hover_color=COLORS['bg_hover'],
            anchor="w",
            command=lambda view=actual_id: self._handle_click(view)
        )
        btn.pack(fill="x", padx=10, pady=pady)
        self.nav_items[actual_id] = btn

    def _handle_click(self, view_id: str):
        self.set_active(view_id)
        self.on_navigate(view_id)

    def set_active(self, view_id: str):
        for vid, btn in self.nav_items.items():
            if vid == view_id:
                btn.configure(fg_color=COLORS['accent_dark'], hover_color=COLORS['accent'])
            else:
                btn.configure(fg_color="transparent", hover_color=COLORS['bg_hover'])
