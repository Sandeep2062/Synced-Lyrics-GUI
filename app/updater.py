import os
import sys
import tempfile
import requests
import subprocess
from packaging import version

CURRENT_VERSION = "1.0.0"

def check_for_updates() -> tuple[bool, str, str]:
    try:
        url = "https://api.github.com/repos/Sandeep2062/Synced-Lyrics-GUI/releases/latest"
        resp = requests.get(url, timeout=10)
        resp.raise_for_status()
        data = resp.json()
        
        latest_tag = data.get("tag_name", "v1.0.0").lstrip('v')
        
        if version.parse(latest_tag) > version.parse(CURRENT_VERSION):
            assets = data.get("assets", [])
            for asset in assets:
                if asset.get("name", "").endswith(".exe"):
                    return True, latest_tag, asset.get("browser_download_url")
        return False, latest_tag, ""
    except Exception:
        return False, CURRENT_VERSION, ""

def download_update(url: str, on_progress) -> str:
    resp = requests.get(url, stream=True)
    resp.raise_for_status()
    total_size = int(resp.headers.get('content-length', 0))
    
    fd, temp_path = tempfile.mkstemp(suffix=".exe")
    with os.fdopen(fd, 'wb') as f:
        downloaded = 0
        for chunk in resp.iter_content(chunk_size=8192):
            if chunk:
                f.write(chunk)
                downloaded += len(chunk)
                if total_size:
                    on_progress(downloaded / total_size)
    return temp_path

def apply_update(exe_path: str) -> None:
    current_exe = sys.executable
    bat_path = os.path.join(tempfile.gettempdir(), "update_synced_lyrics.bat")
    
    bat_content = f"""@echo off
timeout /t 2 /nobreak > NUL
move /Y "{exe_path}" "{current_exe}"
start "" "{current_exe}"
del "%~f0"
"""
    with open(bat_path, "w") as f:
        f.write(bat_content)
        
    subprocess.Popen([bat_path], shell=True)
    sys.exit(0)
