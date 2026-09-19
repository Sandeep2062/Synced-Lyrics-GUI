import os
from pathlib import Path
from dataclasses import dataclass
from typing import Optional, Callable
import mutagen

from app.core import lrc_utils

@dataclass 
class TrackInfo:
    audio_path: str
    lrc_path: str
    artist: Optional[str] = None
    title: Optional[str] = None
    album: Optional[str] = None
    track_number: Optional[int] = None
    duration: Optional[float] = None
    status: str = 'missing'  # 'missing' | 'plain' | 'synced' | 'suspicious'
    lrc_content: Optional[str] = None

def scan_directory(root: str, on_progress: Callable) -> list[TrackInfo]:
    supported_exts = {'.flac', '.mp3', '.m4a', '.opus', '.ogg', '.wav', '.wma', '.aac'}
    tracks = []
    count = 0
    for dirpath, _, filenames in os.walk(root):
        for file in filenames:
            ext = os.path.splitext(file)[1].lower()
            if ext in supported_exts:
                count += 1
                audio_path = os.path.join(dirpath, file)
                lrc_path = os.path.splitext(audio_path)[0] + '.lrc'
                on_progress(count, audio_path)
                
                try:
                    meta = mutagen.File(audio_path, easy=True)
                except Exception:
                    meta = None
                
                artist = meta.get('artist', [None])[0] if meta else None
                title = meta.get('title', [None])[0] if meta else None
                album = meta.get('album', [None])[0] if meta else None
                track_number = meta.get('tracknumber', [None])[0] if meta else None
                
                if track_number and isinstance(track_number, str) and '/' in track_number:
                    track_number = track_number.split('/')[0]
                if track_number:
                    try:
                        track_number = int(track_number)
                    except ValueError:
                        track_number = None
                        
                duration = meta.info.length if meta and hasattr(meta, 'info') and hasattr(meta.info, 'length') else None
                
                if not title:
                    filename_no_ext = os.path.splitext(file)[0]
                    if ' - ' in filename_no_ext:
                        artist, title = filename_no_ext.split(' - ', 1)
                    else:
                        title = filename_no_ext

                status = 'missing'
                lrc_content = None
                if os.path.exists(lrc_path):
                    try:
                        with open(lrc_path, 'r', encoding='utf-8') as f:
                            lrc_content = f.read()
                        
                        timestamps_count = lrc_content.count('[')
                        if timestamps_count >= 3:
                            status = 'synced'
                        else:
                            status = 'plain'
                    except Exception:
                        status = 'missing'
                        
                tracks.append(TrackInfo(
                    audio_path=audio_path,
                    lrc_path=lrc_path,
                    artist=artist,
                    title=title,
                    album=album,
                    track_number=track_number,
                    duration=duration,
                    status=status,
                    lrc_content=lrc_content
                ))
                
    return sorted(tracks, key=lambda x: (x.album or '', x.track_number or 0, x.title or ''))

def group_by_album(tracks: list[TrackInfo]) -> dict[str, list[TrackInfo]]:
    grouped = {}
    for t in tracks:
        album_name = t.album or 'Unknown Album'
        grouped.setdefault(album_name, []).append(t)
    return grouped

def get_summary(tracks: list[TrackInfo]) -> dict:
    summary = {'missing': 0, 'plain': 0, 'synced': 0, 'suspicious': 0, 'total': len(tracks)}
    for t in tracks:
        if t.status in summary:
            summary[t.status] += 1
        else:
            summary['missing'] += 1
    return summary
