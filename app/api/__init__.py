"""Lyrics API providers."""
from .base import LyricsResult, RateLimitConfig, BaseProvider
from .lrclib import LRCLibProvider
from .musixmatch import MusixmatchProvider
from .genius import GeniusProvider
from .netease import NetEaseProvider
from .megalobiz import MegalobizProvider
from .manager import ProviderManager

__all__ = [
    "LyricsResult", "RateLimitConfig", "BaseProvider", 
    "LRCLibProvider", "MusixmatchProvider", "GeniusProvider",
    "NetEaseProvider", "MegalobizProvider", "ProviderManager"
]
