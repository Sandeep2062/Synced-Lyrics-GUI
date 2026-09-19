import os
from concurrent.futures import ThreadPoolExecutor
from app.core import lrc_utils

def audit_track(audio_path: str, lrc_path: str) -> tuple[bool, str]:
    if not os.path.exists(lrc_path):
        return False, "No LRC file"
    
    try:
        with open(lrc_path, 'r', encoding='utf-8') as f:
            lrc_content = f.read()
    except Exception:
        return True, "Cannot read LRC file"

    # Call lrc_utils.check_lrc to do the actual check
    is_suspicious, reason = lrc_utils.check_lrc(lrc_content, audio_path)
    return is_suspicious, reason

def audit_batch(tracks, on_progress) -> list[tuple]:
    suspicious_tracks = []
    
    def _audit(idx, track):
        is_suspicious, reason = audit_track(track.audio_path, track.lrc_path)
        if is_suspicious:
            suspicious_tracks.append((track, reason))
        on_progress(idx + 1, len(tracks))
        
    with ThreadPoolExecutor() as executor:
        for idx, track in enumerate(tracks):
            executor.submit(_audit, idx, track)
            
    return suspicious_tracks

def compare_lyrics(existing_lrc: str, new_lrc: str, duration: float, title: str) -> str:
    ext_tags = existing_lrc.count('[')
    new_tags = new_lrc.count('[')
    
    if new_tags > ext_tags + 10:
        return 'new'
    if ext_tags > new_tags + 10:
        return 'existing'
        
    return 'same'
