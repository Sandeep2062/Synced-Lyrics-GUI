import os
import io
import zipfile
import requests
from typing import Callable
from app import constants

class FFmpegManager:
    def __init__(self):
        self.ffmpeg_dir = constants.FFMPEG_DIR
        self.exe_path = os.path.join(self.ffmpeg_dir, 'ffmpeg.exe')

    def is_installed(self) -> bool:
        return os.path.exists(self.exe_path)

    def get_path(self) -> str:
        return self.exe_path

    def download(self, on_progress: Callable) -> bool:
        try:
            url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip"
            response = requests.get(url, stream=True)
            response.raise_for_status()
            
            total_size = int(response.headers.get('content-length', 0))
            downloaded = 0
            
            zip_buffer = io.BytesIO()
            for chunk in response.iter_content(chunk_size=8192):
                if chunk:
                    zip_buffer.write(chunk)
                    downloaded += len(chunk)
                    if total_size:
                        on_progress(downloaded / total_size)
                        
            with zipfile.ZipFile(zip_buffer) as zf:
                for file_info in zf.infolist():
                    if file_info.filename.endswith('ffmpeg.exe'):
                        os.makedirs(self.ffmpeg_dir, exist_ok=True)
                        file_info.filename = 'ffmpeg.exe'
                        zf.extract(file_info, self.ffmpeg_dir)
                        return True
            return False
        except Exception:
            return False

    def ensure_available(self) -> str:
        if self.is_installed():
            if self.ffmpeg_dir not in os.environ['PATH']:
                os.environ['PATH'] += os.pathsep + self.ffmpeg_dir
            return self.exe_path
        
        self.download(lambda p: None)
        if self.ffmpeg_dir not in os.environ['PATH']:
            os.environ['PATH'] += os.pathsep + self.ffmpeg_dir
        return self.exe_path
