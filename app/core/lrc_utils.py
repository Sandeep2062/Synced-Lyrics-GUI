"""Lyrics utilities."""
import os
import re
import tempfile
from difflib import SequenceMatcher
from pathlib import Path
from typing import Optional, List

STAMP_RE = re.compile(r'\[(\d{2,}):(\d{2})(?:\.(\d{2,3}))?\]')
TITLE_TAG_RE = re.compile(r'\[ti:(.+?)\]', re.IGNORECASE)
NOISE_RE = re.compile(
    r"[\(\[][^\)\]]*(feat|ft\.|remaster|version|live|deluxe|edit|mono|stereo|bonus)[^\)\]]*[\)\]]|[-–—]\s*.*?(?:remaster|version|live|deluxe|bonus).*$",
    re.I,
)

def parse_stamps(text: str) -> List[float]:
    stamps = []
    for match in STAMP_RE.finditer(text):
        try:
            m = int(match.group(1))
            s = int(match.group(2))
            ms_str = match.group(3)
            ms = 0
            if ms_str:
                if len(ms_str) == 2:
                    ms = int(ms_str) * 10
                elif len(ms_str) == 3:
                    ms = int(ms_str)
                else:
                    ms = int(ms_str.ljust(3, '0')[:3])
            stamps.append(m * 60 + s + ms / 1000.0)
        except ValueError:
            pass
    return sorted(stamps)

def count_timestamps(text: str) -> int:
    return len(STAMP_RE.findall(text))

def is_synced(text: str) -> bool:
    return count_timestamps(text) >= 3

def is_plain(text: str) -> bool:
    return bool(text.strip()) and count_timestamps(text) < 3

def normalize_title(title: str) -> str:
    title = NOISE_RE.sub('', title)
    title = title.lower()
    title = re.sub(r'[^a-z0-9]', '', title)
    return title

def titles_match(a: str, b: str) -> bool:
    na = normalize_title(a)
    nb = normalize_title(b)
    if not na or not nb:
        return False
    if na in nb or nb in na:
        return True
    return SequenceMatcher(None, na, nb).ratio() > 0.8

def check_lrc(text: str, duration: float, title: str) -> Optional[str]:
    from app.constants import DURATION_TOLERANCE, EARLY_END_GAP, EARLY_END_RATIO, PAST_END_TOLERANCE
    
    if not text.strip():
        return "empty"
        
    stamps = parse_stamps(text)
    if not stamps:
        return "not_synced"
        
    if len(stamps) < 3:
        return "too_few_timestamps"
        
    if duration > 0:
        last_stamp = stamps[-1]
        
        if last_stamp > duration + PAST_END_TOLERANCE:
            return "too_long"
            
        if last_stamp < duration * EARLY_END_RATIO and (duration - last_stamp) > EARLY_END_GAP:
            return "ends_too_early"
            
    if title:
        title_match = TITLE_TAG_RE.search(text)
        if title_match:
            lrc_title = title_match.group(1)
            if not titles_match(title, lrc_title):
                return "title_mismatch"
                
    return None

def read_text(path: str | Path, limit: Optional[int] = None) -> str:
    try:
        with open(path, 'r', encoding='utf-8', errors='ignore') as f:
            if limit:
                return f.read(limit)
            return f.read()
    except Exception:
        return ""

def write_atomic(path: str | Path, text: str) -> None:
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temp_path = tempfile.mkstemp(dir=path.parent, prefix=path.name + ".")
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as f:
            f.write(text)
        os.replace(temp_path, path)
    except Exception:
        try:
            os.unlink(temp_path)
        except Exception:
            pass
        raise

def fmt_duration(seconds: float) -> str:
    m, s = divmod(int(seconds), 60)
    return f"{m}:{s:02d}"
