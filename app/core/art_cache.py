"""Embedded album artwork extractor and high-performance thumbnail cache."""
import os
import io
import hashlib
from typing import Optional, Tuple
from PIL import Image, ImageDraw
import customtkinter as ctk
import mutagen
from mutagen.id3 import ID3, APIC
from mutagen.flac import FLAC
from mutagen.mp4 import MP4

from app.constants import APPDATA_DIR

ART_CACHE_DIR = APPDATA_DIR / 'cache' / 'art'
ART_CACHE_DIR.mkdir(parents=True, exist_ok=True)

# In-memory LRU cache: (cache_key, size) -> ctk.CTkImage
_MEM_CACHE: dict = {}
_MAX_MEM_ENTRIES = 500

# Reusable default placeholder image
_PLACEHOLDER_CACHE: dict = {}

def _get_placeholder(size: Tuple[int, int]) -> ctk.CTkImage:
    if size in _PLACEHOLDER_CACHE:
        return _PLACEHOLDER_CACHE[size]
    w, h = size
    img = Image.new('RGBA', (w, h), (18, 20, 29, 255))
    draw = ImageDraw.Draw(img)
    # Subtle border
    draw.rounded_rectangle([(0, 0), (w - 1, h - 1)], radius=6, outline=(35, 39, 58, 255), width=1)
    
    # Draw minimalist vinyl grooves / musical symbol placeholder
    cx, cy = w // 2, h // 2
    r_max = min(w, h) // 3
    if r_max > 8:
        # Concentric vinyl rings
        draw.ellipse([(cx - r_max, cy - r_max), (cx + r_max, cy + r_max)], outline=(28, 32, 48, 255), width=1)
        r_mid = int(r_max * 0.65)
        draw.ellipse([(cx - r_mid, cy - r_mid), (cx + r_mid, cy + r_mid)], outline=(35, 40, 60, 255), width=1)
        r_inner = max(2, int(r_max * 0.3))
        draw.ellipse([(cx - r_inner, cy - r_inner), (cx + r_inner, cy + r_inner)], fill=(139, 92, 246, 180)) # Violet center
    else:
        draw.ellipse([(cx - 3, cy - 3), (cx + 3, cy + 3)], fill=(139, 92, 246, 180))

    ctk_img = ctk.CTkImage(light_image=img, dark_image=img, size=size)
    _PLACEHOLDER_CACHE[size] = ctk_img
    return ctk_img

def extract_raw_cover(audio_path: str) -> Optional[bytes]:
    """Extract raw cover artwork bytes from ID3, FLAC, MP4 tags or directory images."""
    if not os.path.exists(audio_path):
        return None

    ext = os.path.splitext(audio_path)[1].lower()
    try:
        if ext == '.mp3':
            try:
                id3 = ID3(audio_path)
                for tag in id3.values():
                    if isinstance(tag, APIC):
                        return tag.data
            except Exception:
                pass
        elif ext == '.flac':
            try:
                flac = FLAC(audio_path)
                if flac.pictures:
                    return flac.pictures[0].data
            except Exception:
                pass
        elif ext in ('.m4a', '.mp4', '.aac'):
            try:
                mp4 = MP4(audio_path)
                covr = mp4.tags.get('covr') if mp4.tags else None
                if covr and len(covr) > 0:
                    return bytes(covr[0])
            except Exception:
                pass
        else:
            # Generic fallback with mutagen.File
            try:
                meta = mutagen.File(audio_path)
                if hasattr(meta, 'pictures') and meta.pictures:
                    return meta.pictures[0].data
            except Exception:
                pass
    except Exception:
        pass

    # If embedded art is absent, check parent directory for common album art files
    try:
        parent_dir = os.path.dirname(audio_path)
        if os.path.isdir(parent_dir):
            common_names = {
                'cover.jpg', 'cover.jpeg', 'cover.png',
                'folder.jpg', 'folder.png', 'folder.jpeg',
                'album.jpg', 'album.png', 'front.jpg', 'front.png'
            }
            for fname in os.listdir(parent_dir):
                if fname.lower() in common_names:
                    candidate = os.path.join(parent_dir, fname)
                    if os.path.isfile(candidate) and os.path.getsize(candidate) > 0:
                        with open(candidate, 'rb') as f:
                            return f.read()
    except Exception:
        pass

    return None

def get_thumbnail(audio_path: str, size: Tuple[int, int] = (40, 40)) -> ctk.CTkImage:
    """
    Get a cached CTkImage thumbnail for a track, extracting embedded art if present.
    Returns a sleek dark placeholder if no artwork is embedded.
    """
    if not audio_path:
        return _get_placeholder(size)

    cache_key = hashlib.md5(audio_path.encode('utf-8', errors='ignore')).hexdigest()
    mem_key = (cache_key, size[0], size[1])
    
    if mem_key in _MEM_CACHE:
        return _MEM_CACHE[mem_key]

    disk_path = ART_CACHE_DIR / f"{cache_key}_{size[0]}x{size[1]}.png"
    
    # Check disk cache
    if disk_path.exists():
        try:
            pil_img = Image.open(str(disk_path))
            ctk_img = ctk.CTkImage(light_image=pil_img, dark_image=pil_img, size=size)
            if len(_MEM_CACHE) > _MAX_MEM_ENTRIES:
                _MEM_CACHE.pop(next(iter(_MEM_CACHE)))
            _MEM_CACHE[mem_key] = ctk_img
            return ctk_img
        except Exception:
            pass

    # Extract artwork from file
    raw_data = extract_raw_cover(audio_path)
    if raw_data:
        try:
            pil_img = Image.open(io.BytesIO(raw_data)).convert('RGBA')
            pil_img = pil_img.resize(size, Image.Resampling.LANCZOS)
            
            # Apply rounded corners to match modern card UI
            mask = Image.new('L', size, 0)
            draw = ImageDraw.Draw(mask)
            draw.rounded_rectangle([(0, 0), (size[0] - 1, size[1] - 1)], radius=4, fill=255)
            pil_img.putalpha(mask)
            
            # Save to disk
            pil_img.save(str(disk_path), format='PNG')
            
            ctk_img = ctk.CTkImage(light_image=pil_img, dark_image=pil_img, size=size)
            if len(_MEM_CACHE) > _MAX_MEM_ENTRIES:
                _MEM_CACHE.pop(next(iter(_MEM_CACHE)))
            _MEM_CACHE[mem_key] = ctk_img
        except Exception:
            pass

    # Fallback to placeholder
    placeholder = _get_placeholder(size)
    _MEM_CACHE[mem_key] = placeholder
    return placeholder

import concurrent.futures
from typing import Callable, Any

_THUMB_EXECUTOR = concurrent.futures.ThreadPoolExecutor(max_workers=4, thread_name_prefix="ArtLoader")


def load_thumbnail_async(
    audio_path: str,
    size: Tuple[int, int] = (40, 40),
    target_widget: Optional[Any] = None,
    callback: Optional[Callable[[ctk.CTkImage], None]] = None
) -> ctk.CTkImage:
    """
    Asynchronously loads thumbnail without blocking the main UI thread.
    - If in memory, sets image immediately and returns it.
    - If not in memory, sets placeholder immediately, extracts artwork in background,
      and updates target_widget or calls callback safely on main thread.
    """
    placeholder = _get_placeholder(size)
    if not audio_path or not os.path.exists(audio_path):
        if target_widget and hasattr(target_widget, 'configure'):
            target_widget.configure(image=placeholder)
        if callback:
            callback(placeholder)
        return placeholder


    cache_key = hashlib.md5(audio_path.encode('utf-8', errors='ignore')).hexdigest()
    mem_key = (cache_key, size[0], size[1])

    if mem_key in _MEM_CACHE:
        cached = _MEM_CACHE[mem_key]
        if target_widget and hasattr(target_widget, 'configure'):
            target_widget.configure(image=cached)
        if callback:
            callback(cached)
        return cached

    # Check if file exists on disk
    disk_path = ART_CACHE_DIR / f"{cache_key}_{size[0]}x{size[1]}.png"
    if disk_path.exists():
        try:
            pil_img = Image.open(str(disk_path))
            ctk_img = ctk.CTkImage(light_image=pil_img, dark_image=pil_img, size=size)
            if len(_MEM_CACHE) > _MAX_MEM_ENTRIES:
                _MEM_CACHE.pop(next(iter(_MEM_CACHE)))
            _MEM_CACHE[mem_key] = ctk_img
            if target_widget and hasattr(target_widget, 'configure'):
                target_widget.configure(image=ctk_img)
            if callback:
                callback(ctk_img)
            return ctk_img
        except Exception:
            pass

    # Show placeholder immediately while worker decodes
    if target_widget and hasattr(target_widget, 'configure'):
        target_widget.configure(image=placeholder)
        target_widget._current_audio_art = audio_path

    def _worker():
        img = get_thumbnail(audio_path, size)
        if target_widget and hasattr(target_widget, 'after'):
            def _apply():
                try:
                    if getattr(target_widget, '_current_audio_art', None) == audio_path:
                        target_widget.configure(image=img)
                except Exception:
                    pass
            try:
                target_widget.after(0, _apply)
            except Exception:
                pass
        if callback:
            callback(img)

    _THUMB_EXECUTOR.submit(_worker)
    return placeholder

