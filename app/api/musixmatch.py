"""Musixmatch lyrics provider supporting built-in token rotation and official API keys."""
import urllib.request
import urllib.parse
import json
import re
from typing import Optional
from .base import BaseProvider, LyricsResult, RateLimitConfig

try:
    from syncedlyrics.providers import Musixmatch as SLMusixmatch
except ImportError:
    SLMusixmatch = None

class MusixmatchProvider(BaseProvider):
    name = 'Musixmatch'
    can_provide_synced = True
    requires_api_key = False
    
    def __init__(self):
        self.api_key: Optional[str] = None
        super().__init__()
        
    @property
    def rate_config(self) -> RateLimitConfig:
        # If user provided official API key, respect 10 req/min and 2,000 req/day
        if self.api_key:
            return RateLimitConfig(requests_per_minute=10, requests_per_day=2000, base_interval=6.0)
        # Built-in token rotation mode
        return RateLimitConfig(requests_per_minute=30, requests_per_day=0, base_interval=2.0)

    def set_api_key(self, key: str) -> None:
        clean = key.strip() if key else ""
        self.api_key = clean if clean else None
        self.requires_api_key = bool(self.api_key)

    def search_lyrics(self, artist: str, title: str, duration: float | None = None) -> LyricsResult:
        self._limiter.wait()
        
        try:
            # Mode A: User provided custom API key
            if self.api_key:
                # 1. Try to get synchronized subtitles first
                sub_url = (
                    f"https://api.musixmatch.com/ws/1.1/matcher.subtitle.get?"
                    f"q_track={urllib.parse.quote(title)}&q_artist={urllib.parse.quote(artist)}&apikey={self.api_key}"
                )
                req = urllib.request.Request(sub_url, headers={'User-Agent': 'SyncedLyricsGUI/1.0.0'})
                with urllib.request.urlopen(req, timeout=12) as response:
                    data = json.loads(response.read().decode())
                    header = data.get('message', {}).get('header', {})
                    status_code = header.get('status_code', 0)
                    
                    if status_code == 429:
                        self._limiter.penalize(60)
                        if self._on_rate_limit:
                            self._on_rate_limit(self.name, 60)
                        return LyricsResult()
                        
                    body = data.get('message', {}).get('body', {})
                    subtitle = body.get('subtitle', {})
                    sub_body = subtitle.get('subtitle_body')
                    if sub_body:
                        self._limiter.relax()
                        return LyricsResult(
                            synced=sub_body,
                            plain=self._extract_plain(sub_body),
                            source="Musixmatch (API)",
                            confidence=0.9
                        )
                
                # 2. Fallback to plain lyrics with API key
                lyr_url = (
                    f"https://api.musixmatch.com/ws/1.1/matcher.lyrics.get?"
                    f"q_track={urllib.parse.quote(title)}&q_artist={urllib.parse.quote(artist)}&apikey={self.api_key}"
                )
                req = urllib.request.Request(lyr_url, headers={'User-Agent': 'SyncedLyricsGUI/1.0.0'})
                with urllib.request.urlopen(req, timeout=12) as response:
                    data = json.loads(response.read().decode())
                    body = data.get('message', {}).get('body', {})
                    lyrics = body.get('lyrics', {})
                    lyr_body = lyrics.get('lyrics_body')
                    if lyr_body:
                        self._limiter.relax()
                        return LyricsResult(
                            plain=lyr_body,
                            source="Musixmatch (API)",
                            confidence=0.7
                        )

            # Mode B: Built-in token rotation provider (Default & Recommended)
            if SLMusixmatch:
                provider = SLMusixmatch(lang=None, enhanced=False)
                query = f"{title} {artist}".strip()
                lrc = provider.get_lrc(query)
                self._limiter.relax()
                if lrc:
                    return LyricsResult(
                        synced=lrc,
                        plain=self._extract_plain(lrc),
                        source="Musixmatch (Built-in)",
                        confidence=0.85
                    )

        except urllib.error.HTTPError as e:
            if e.code == 429:
                retry_after = int(e.headers.get('Retry-After', 60))
                self._limiter.penalize(retry_after)
                if self._on_rate_limit:
                    self._on_rate_limit(self.name, retry_after)
        except Exception:
            pass
            
        return LyricsResult()

    def _extract_plain(self, lrc: str) -> str:
        return re.sub(r'\[.*?\]', '', lrc).strip()
