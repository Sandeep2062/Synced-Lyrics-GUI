"""Main application window with LRCGET top header navigation and persistent player."""
import os
import customtkinter as ctk
import tkinter.filedialog as filedialog
from PIL import Image

from app.ui.theme import COLORS, FONTS
from app.ui.widgets.status_bar import StatusBar
from app.ui.widgets.now_playing_bar import NowPlayingBar
from app.ui.widgets.lyrics_drawer import LyricsDrawer

from app.ui.views.library_view import LibraryView
from app.ui.views.albums_view import AlbumsView
from app.ui.views.artists_view import ArtistsView
from app.ui.views.download_view import DownloadView
from app.ui.views.settings_view import SettingsView
from app.ui.views.logs_view import LogsView

from app.audio_player import AudioPlayer
from app.core.fetcher import BatchFetcher
from app.ffmpeg_manager import FFmpegManager
from app.config import get_config
from app.core.library_db import LibraryDB
from app.api.manager import ProviderManager
from app.constants import DB_FILE, APPDATA_DIR

class AppWindow(ctk.CTk):
    """
    Main application coordinator matching LRCGET:
    - Top header: Logo + 'Synced Lyrics' + Tabs (Tracks, Albums, Artists, Providers, Settings) + 'DOWNLOAD ALL LYRICS' pink pill
    - Center container: Active tab view
    - Slide-up synced lyrics drawer
    - Persistent bottom player bar
    """
    def __init__(self, config=None, db=None, provider_manager=None):
        super().__init__()
        
        self.config = config or get_config()
        self.db = db or LibraryDB(str(DB_FILE))
        self.provider_manager = provider_manager or ProviderManager()
        self.provider_manager.configure_api_keys(self.config.api_keys)
        
        self.audio_player = AudioPlayer()
        self.audio_player.set_volume(self.config.volume)
        self.fetcher = BatchFetcher(self.provider_manager, self.db, self.config)
        self.ffmpeg_manager = FFmpegManager()
        
        self.title("Synced Lyrics")
        self.geometry(self.config.window_geometry or "1280x820")
        self.minsize(980, 640)
        
        # Window Icon
        icon_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "assets", "icon.ico")
        if os.path.exists(icon_path):
            try:
                self.iconbitmap(icon_path)
            except Exception:
                pass
                
        # Appearance mode: pure black dark mode
        ctk.set_appearance_mode("dark")
        self.configure(fg_color=COLORS['bg_primary'])
        
        # Master Layout (Rows: 0 = Top Header, 1 = Main Content, 2 = Lyrics Drawer (optional), 3 = Player, 4 = Status)
        self.grid_columnconfigure(0, weight=1)
        self.grid_rowconfigure(1, weight=1)
        
        # 1. Top Header Bar (Matching LRCGET Header)
        self._build_header_bar()
        
        # 2. Main Content Frame
        self.content_frame = ctk.CTkFrame(self, fg_color=COLORS['bg_primary'], corner_radius=0)
        self.content_frame.grid(row=1, column=0, sticky="nsew")
        self.content_frame.grid_rowconfigure(0, weight=1)
        self.content_frame.grid_columnconfigure(0, weight=1)
        
        # 3. Views Dictionary
        self.views = {
            'tracks': LibraryView(self.content_frame, app_window=self),
            'albums': AlbumsView(self.content_frame, app_window=self),
            'artists': ArtistsView(self.content_frame, app_window=self),
            'download': DownloadView(self.content_frame, app_window=self),
            'logs': LogsView(self.content_frame, app_window=self),
            'settings': SettingsView(self.content_frame, app_window=self)
        }
        
        # 4. Slide-Up Lyrics Drawer (Hidden by default)
        self.lyrics_drawer = LyricsDrawer(self, app_window=self, on_close=self.toggle_lyrics_drawer)
        self.lyrics_drawer_open = False
        
        # 5. Persistent Bottom Now-Playing Bar
        self.now_playing = NowPlayingBar(self, app_window=self, on_toggle_lyrics=self.toggle_lyrics_drawer)
        self.now_playing.grid(row=3, column=0, sticky="ew")
        
        # 6. Status Bar
        self.status_bar = StatusBar(self)
        self.status_bar.grid(row=4, column=0, sticky="ew")
        
        # Active View State
        self.current_view = None
        self.switch_view(self.config.last_view or "tracks")
        
        # Load cached tracks immediately (0ms instant boot)
        self.after(10, self._on_initial_load)

    def _build_header_bar(self):
        self.header_frame = ctk.CTkFrame(
            self, 
            fg_color=COLORS['bg_toolbar'], 
            height=54, 
            corner_radius=0, 
            border_width=1, 
            border_color=COLORS['border']
        )
        self.header_frame.grid(row=0, column=0, sticky="ew")
        self.header_frame.pack_propagate(False)
        
        # Left: App Logo Icon + App Title
        logo_frame = ctk.CTkFrame(self.header_frame, fg_color="transparent")
        logo_frame.pack(side="left", padx=(16, 24))
        
        logo_png = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "assets", "icon.png")
        if os.path.exists(logo_png):
            try:
                pil_logo = Image.open(logo_png).resize((28, 28), Image.Resampling.LANCZOS)
                self.logo_img = ctk.CTkImage(light_image=pil_logo, dark_image=pil_logo, size=(28, 28))
                ctk.CTkLabel(logo_frame, text="", image=self.logo_img, width=28, height=28).pack(side="left", padx=(0, 8))
            except Exception:
                pass
                
        self.app_title_lbl = ctk.CTkLabel(
            logo_frame, 
            text="SYNCED LYRICS", 
            font=FONTS['heading'], 
            text_color=COLORS['accent']
        )
        self.app_title_lbl.pack(side="left")

        # Center: Top Navigation Tabs (Tracks, Albums, Artists, Progress, Settings)
        self.tabs_frame = ctk.CTkFrame(self.header_frame, fg_color="transparent")
        self.tabs_frame.pack(side="left", padx=10)
        
        self.nav_tabs = {}
        for tab_id, label in [
            ('tracks', 'Tracks'),
            ('albums', 'Albums'),
            ('artists', 'Artists'),
            ('download', 'Downloads'),
            ('logs', 'Logs'),
            ('settings', 'Settings')
        ]:
            btn = ctk.CTkButton(
                self.tabs_frame,
                text=label,
                font=FONTS['tab'],
                fg_color="transparent",
                text_color=COLORS['text_muted'],
                hover_color=COLORS['bg_button'],
                corner_radius=4,
                width=80,
                height=32,
                command=lambda v=tab_id: self.switch_view(v)
            )
            btn.pack(side="left", padx=3)
            self.nav_tabs[tab_id] = btn

        # Right Action Buttons: 'DOWNLOAD ALL LYRICS' Pink Pill & 'Add Folder'
        self.header_actions = ctk.CTkFrame(self.header_frame, fg_color="transparent")
        self.header_actions.pack(side="right", padx=16)
        
        self.dl_all_btn = ctk.CTkButton(
            self.header_actions, 
            text="⬇ DOWNLOAD ALL LYRICS", 
            font=FONTS['small_bold'],
            fg_color=COLORS['accent'], 
            hover_color=COLORS['accent_hover'],
            text_color="#FFFFFF",
            corner_radius=16, # Pill shape
            height=32,
            width=180,
            command=self._on_download_all_clicked
        )
        self.dl_all_btn.pack(side="left", padx=6)
        
        self.add_folder_btn = ctk.CTkButton(
            self.header_actions, 
            text="+ Add Folder", 
            font=FONTS['small_bold'],
            fg_color=COLORS['bg_button'], 
            hover_color=COLORS['bg_button_hover'],
            text_color=COLORS['text_primary'],
            corner_radius=16,
            height=32,
            width=100,
            command=self._on_add_folder_clicked
        )
        self.add_folder_btn.pack(side="left", padx=4)

    def switch_view(self, view_name: str):
        if view_name not in self.views:
            return
            
        if self.current_view and self.current_view in self.views:
            self.views[self.current_view].grid_forget()
            
        # Update tab styling
        for tid, btn in self.nav_tabs.items():
            if tid == view_name:
                btn.configure(text_color=COLORS['accent'], fg_color=COLORS['bg_button'])
            else:
                btn.configure(text_color=COLORS['text_muted'], fg_color="transparent")
                
        self.current_view = view_name
        self.views[view_name].grid(row=0, column=0, sticky="nsew")
        self.config.last_view = view_name
        
        # Lazy load data for tabs (instant: only on first visit)
        if view_name == 'albums':
            self.views['albums'].load_albums(force=False)
        elif view_name == 'artists':
            self.views['artists'].load_artists(force=False)


    def toggle_lyrics_drawer(self):
        """Toggle slide-up lyrics panel matching screenshot 2."""
        if self.lyrics_drawer_open:
            self.lyrics_drawer.grid_forget()
            self.lyrics_drawer_open = False
        else:
            self.lyrics_drawer.grid(row=2, column=0, sticky="nsew")
            self.lyrics_drawer_open = True
            
            # Load lyrics for now playing track
            if self.now_playing.current_track:
                t = self.now_playing.current_track
                lrc_path = getattr(t, 'lrc_path', None) or (t.get('lrc_path') if isinstance(t, dict) else '')
                from app.core import lrc_utils
                text = lrc_utils.read_text(lrc_path) if lrc_path and os.path.exists(lrc_path) else ''
                self.lyrics_drawer.load_lyrics(text)

    def play_track(self, track: Any):
        """Play track via bottom player bar and load lyrics into drawer."""
        self.now_playing.set_track(track)
        
        # Load lyrics into drawer
        lrc_path = getattr(track, 'lrc_path', None) or (track.get('lrc_path') if isinstance(track, dict) else '')
        from app.core import lrc_utils
        text = lrc_utils.read_text(lrc_path) if lrc_path and os.path.exists(lrc_path) else ''
        self.lyrics_drawer.load_lyrics(text)

    def start_batch_download(self, tracks: list, mode: str):
        self.switch_view('download')
        self.views['download'].start_download(tracks, mode)

    def download_single_track(self, track: Any):
        from app.core.scanner import TrackInfo
        t_obj = TrackInfo(
            audio_path=getattr(track, 'audio_path', None) or track.get('audio_path'),
            lrc_path=getattr(track, 'lrc_path', None) or track.get('lrc_path'),
            artist=getattr(track, 'artist', None) or track.get('artist'),
            title=getattr(track, 'title', None) or track.get('title'),
            album=getattr(track, 'album', None) or track.get('album'),
            duration=getattr(track, 'duration', None) or track.get('duration'),
            status=getattr(track, 'status', None) or track.get('lrc_status', 'missing')
        )
        self.start_batch_download([t_obj], mode='replace')

    def _on_download_all_clicked(self):
        tracks = self.views['tracks'].all_tracks
        if not tracks:
            self.status_bar.set_status("Please add a music folder first.")
            return
            
        from app.core.scanner import TrackInfo
        track_objs = [
            TrackInfo(
                audio_path=t['audio_path'],
                lrc_path=t['lrc_path'],
                artist=t.get('artist'),
                title=t.get('title'),
                album=t.get('album'),
                duration=t.get('duration'),
                status=t.get('lrc_status', 'missing')
            )
            for t in tracks
        ]
        self.start_batch_download(track_objs, mode="smart")

    def _on_add_folder_clicked(self):
        chosen = filedialog.askdirectory(title="Select Music Folder")
        if chosen:
            self.config.add_directory(chosen)
            self.db.add_directory(chosen)
            self.status_bar.set_status(f"Added folder: {os.path.basename(chosen)}. Scanning started...")
            self.views['tracks'].trigger_auto_scan([chosen])

    def _on_initial_load(self):
        # 1. Load instantly from DB
        self.views['tracks'].load_from_db()
        
        # 2. Check if folders need scanning
        dirs = self.config.directories
        if dirs:
            self.views['tracks'].trigger_auto_scan(dirs)
        
        # 3. Check ffmpeg
        if self.ffmpeg_manager.is_installed():
            self.ffmpeg_manager.ensure_available()

    def on_closing(self):
        try:
            self.config.window_geometry = self.geometry()
            self.config.save()
        except Exception:
            pass
        try:
            if self.audio_player:
                self.audio_player.cleanup()
        except Exception:
            pass
        try:
            if self.fetcher and self.fetcher.is_running:
                self.fetcher.cancel()
        except Exception:
            pass
        try:
            self.quit()
        except Exception:
            pass
        self.destroy()

if __name__ == "__main__":
    app = AppWindow()
    app.protocol("WM_DELETE_WINDOW", app.on_closing)
    app.mainloop()
