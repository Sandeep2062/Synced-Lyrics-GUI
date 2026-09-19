"""Engine Manager for managing external dependencies (FFmpeg and syncedlyrics)."""
import os
import sys
import json
import urllib.request
import subprocess
import importlib.metadata
from typing import Tuple, Callable, Optional
from packaging import version

from app import constants
from app.ffmpeg_manager import FFmpegManager

class EngineManager:
    """Manages syncedlyrics package version and FFmpeg binaries."""
    
    def __init__(self):
        self.ffmpeg = FFmpegManager()
        
    def get_syncedlyrics_version(self) -> str:
        """Get locally installed syncedlyrics version."""
        try:
            return importlib.metadata.version('syncedlyrics')
        except Exception:
            try:
                import syncedlyrics
                return getattr(syncedlyrics, '__version__', 'unknown')
            except Exception:
                return "Not installed"

    def check_syncedlyrics_update(self) -> Tuple[bool, str, str]:
        """
        Check PyPI for the latest version of syncedlyrics.
        Returns (has_update, current_version, latest_version).
        """
        current_v = self.get_syncedlyrics_version()
        try:
            url = "https://pypi.org/pypi/syncedlyrics/json"
            req = urllib.request.Request(url, headers={'User-Agent': 'SyncedLyricsGUI/1.0.0'})
            with urllib.request.urlopen(req, timeout=10) as response:
                data = json.loads(response.read().decode())
                latest_v = data.get('info', {}).get('version', current_v)
                
                if current_v != "Not installed" and current_v != "unknown":
                    has_update = version.parse(latest_v) > version.parse(current_v)
                    return has_update, current_v, latest_v
                return False, current_v, latest_v
        except Exception:
            return False, current_v, current_v

    def update_syncedlyrics(self, on_progress: Optional[Callable[[str], None]] = None) -> Tuple[bool, str]:
        """
        Update or install syncedlyrics via pip.
        Works when running in a Python environment.
        """
        if getattr(sys, 'frozen', False):
            # Running inside a PyInstaller .exe
            return False, "Syncedlyrics engine is embedded inside the standalone .exe binary. Updating the .exe updates all engines."
            
        try:
            if on_progress:
                on_progress("Running: pip install -U syncedlyrics...")
                
            cmd = [sys.executable, "-m", "pip", "install", "--upgrade", "syncedlyrics"]
            result = subprocess.run(cmd, capture_output=True, text=True, check=True)
            new_version = self.get_syncedlyrics_version()
            return True, f"Successfully upgraded syncedlyrics to v{new_version}!"
        except Exception as e:
            return False, f"Failed to upgrade syncedlyrics: {e}"

    def ensure_engines(self, on_progress: Optional[Callable[[str], None]] = None) -> dict:
        """
        Verify and auto-download FFmpeg if missing, and check syncedlyrics.
        """
        results = {
            "ffmpeg_installed": False,
            "ffmpeg_path": "",
            "syncedlyrics_version": self.get_syncedlyrics_version()
        }
        
        # 1. FFmpeg
        if self.ffmpeg.is_installed():
            results["ffmpeg_installed"] = True
            results["ffmpeg_path"] = self.ffmpeg.get_path()
            self.ffmpeg.ensure_available()
        else:
            if on_progress:
                on_progress("FFmpeg not detected. Downloading static FFmpeg binary...")
            success = self.ffmpeg.download(lambda p: None)
            if success:
                results["ffmpeg_installed"] = True
                results["ffmpeg_path"] = self.ffmpeg.get_path()
                self.ffmpeg.ensure_available()
                if on_progress:
                    on_progress("FFmpeg downloaded and configured successfully!")
            else:
                if on_progress:
                    on_progress("Could not auto-download FFmpeg. Audio playback will use system codecs.")
                    
        return results
