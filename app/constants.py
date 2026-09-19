"""Constants for the Synced Lyrics GUI application."""
import os
import sys
from pathlib import Path

from app import __version__

APP_NAME = "SyncedLyricsGUI"
APP_VERSION = __version__

AUDIO_EXTENSIONS = ('.flac', '.mp3', '.m4a', '.opus', '.ogg', '.wav', '.wma', '.aac')

def get_appdata_dir() -> Path:
    if sys.platform == "win32":
        appdata = os.environ.get("APPDATA")
        if appdata:
            return Path(appdata) / APP_NAME
    return Path.home() / f".{APP_NAME}"

APPDATA_DIR = get_appdata_dir()
APPDATA_DIR.mkdir(parents=True, exist_ok=True)

CONFIG_FILE = APPDATA_DIR / 'config.json'
DB_FILE = APPDATA_DIR / 'library.db'
FFMPEG_DIR = APPDATA_DIR / 'ffmpeg'
LOG_DIR = APPDATA_DIR / 'logs'

LOG_DIR.mkdir(parents=True, exist_ok=True)

LRCLIB_API_BASE = 'https://lrclib.net/api'
LRCLIB_HEADERS = {
    'User-Agent': f'SyncedLyricsGUI/{APP_VERSION} (https://github.com/Sandeep2062/Synced-Lyrics-GUI)'
}

MUSIXMATCH_API_BASE = 'https://api.musixmatch.com/ws/1.1/'
GENIUS_API_BASE = 'https://api.genius.com'

GITHUB_REPO = 'Sandeep2062/Synced-Lyrics-GUI'
GITHUB_RELEASES_URL = f'https://api.github.com/repos/{GITHUB_REPO}/releases/latest'

DURATION_TOLERANCE = 3
PAST_END_TOLERANCE = 10
EARLY_END_RATIO = 0.5
EARLY_END_GAP = 45
