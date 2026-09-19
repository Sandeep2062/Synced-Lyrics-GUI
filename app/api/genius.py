"""Genius plain-text lyrics fallback provider supporting built-in and custom API tokens."""
import urllib.request
import urllib.parse
import json
from typing import Optional
from .base import BaseProvider, LyricsResult, RateLimitConfig

try:
    from syncedlyrics.providers import Genius as SLGenius
except ImportError:
    SLGenius = None

class GeniusProvider(BaseProvider):
    name = 'Genius'
    can_provide_synced = False
    requires_api_key = False
    
    def __init__(self):
        self.api_token: Optional[str] = None
        super().__init__()
        
    @property
    def rate_config(self) -> RateLimitConfig:
        return RateLimitConfig(requests_per_minute=10, requests_per_day=0, base_interval=6.0)

    def set_api_key(self, key: str) -> None:
        clean = key.strip() if key else ""
        self.api_token = clean if clean else None
        self.requires_api_key = bool(self.api_token)

    def search_lyrics(self, artist: str, title: str, duration: float | None = None) -> LyricsResult:
        self._limiter.wait()
        query = f"{title} {artist}".strip()
        
        try:
            # Mode A: User supplied custom Genius Client Access Token
            if self.api_token:
                search_url = f"https://api.genius.com/search?q={urllib.parse.quote(query)}"
                req = urllib.request.Request(search_url, headers={
                    'Authorization': f'Bearer {self.api_token}',
                    'User-Agent': 'SyncedLyricsGUI/1.0.0'
                })
                with urllib.request.urlopen(req, timeout=12) as response:
                    data = json.loads(response.read().decode())
                    hits = data.get('response', {}).get('hits', [])
                    if hits and SLGenius:
                        # Top hit found with verified API token, use internal scraper for lyric text
                        provider = SLGenius()
                        lrc = provider.get_lrc(query)
                        self._limiter.relax()
                        if lrc:
                            return LyricsResult(
                                plain=lrc,
                                source="Genius (Token Auth)",
                                confidence=0.75
                            )

            # Mode B: Built-in Genius scraper (Default)
            if SLGenius:
                provider = SLGenius()
                lrc = provider.get_lrc(query)
                self._limiter.relax()
                if lrc:
                    return LyricsResult(
                        plain=lrc,
                        source="Genius (Built-in)",
                        confidence=0.7
                    )

        except urllib.error.HTTPError as e:
            if e.code == 429:
                self._limiter.penalize(60)
                if self._on_rate_limit:
                    self._on_rate_limit(self.name, 60)
        except Exception:
            pass
            
        return LyricsResult()
