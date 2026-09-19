"""Application entry point and bootstrap."""
import sys
import os

# Ensure the root repository directory is in sys.path
_repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if _repo_root not in sys.path:
    sys.path.insert(0, _repo_root)

import customtkinter as ctk

def main():
    """Initialize and run the application."""
    # Set dark appearance mode
    ctk.set_appearance_mode('dark')
    ctk.set_default_color_theme('blue')
    
    from app.config import get_config
    from app.core.library_db import LibraryDB
    from app.constants import DB_FILE
    from app.api.manager import ProviderManager
    from app.ui.app_window import AppWindow
    
    # Initialize Core Services
    config = get_config()
    db = LibraryDB(str(DB_FILE))
    provider_manager = ProviderManager()
    
    # Configure API keys from settings
    provider_manager.configure_api_keys(config.api_keys)
    
    # Create and run app window
    app = AppWindow(config=config, db=db, provider_manager=provider_manager)
    
    app.protocol('WM_DELETE_WINDOW', app.on_closing)
    app.mainloop()

if __name__ == '__main__':
    main()
