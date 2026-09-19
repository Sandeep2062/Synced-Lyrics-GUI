"""LRCLib lyrics provider supporting public or custom self-hosted instances."""
import urllib.request
import urllib.parse
import json
import re
from typing import Optional
from .base import BaseProvider, LyricsResult, RateLimitConfig

class LRCLibProvider(BaseProvider):
    name = 'LRCLib'
    can_provide_synced = True
    requires_api_key = False
    
    def __init__(self, instance_url: str = "https://lrclib.net"):
        super().__init__()
        self.instance_url = instance_url.rstrip('/')
        
    @property
    def rate_config(self) -> RateLimitConfig:
        return RateLimitConfig(requests_per_minute=60, requests_per_day=0, base_interval=0.5)

    def set_api_key(self, key: str) -> None:
        """Allow setting a custom instance URL (e.g. self-hosted instance)."""
        if key and key.strip():
            clean = key.strip().rstrip('/')
            if not clean.startswith('http'):
                clean = 'https://' + clean
            self.instance_url = clean
        else:
            self.instance_url = "https://lrclib.net"

    def _clean_title(self, title: str) -> str:
        # Remove text in parentheses like (feat. X), (Remastered)
        title = re.sub(r'\(.*?(feat|remaster|radio|edit|mix|version).*?\)', '', title, flags=re.IGNORECASE)
        # Remove text after - like - Remastered
        title = re.sub(r'-.*?(remaster|radio|edit|mix|version).*$', '', title, flags=re.IGNORECASE)
        return title.strip()

    def search_lyrics(self, artist: str, title: str, duration: float | None = None) -> LyricsResult:
        titles_to_try = [title]
        cleaned = self._clean_title(title)
        if cleaned != title and cleaned:
            titles_to_try.append(cleaned)
            
        base_api = f"{self.instance_url}/api/search"
        
        for t in titles_to_try:
            try:
                self._limiter.wait()
                params = {'track_name': t}
                if artist:
                    params['artist_name'] = artist
                url = f"{base_api}?{urllib.parse.urlencode(params)}"
                
                req = urllib.request.Request(
                    url, 
                    headers={'User-Agent': 'SyncedLyricsGUI/1.0.0 (https://github.com/Sandeep2062/Synced-Lyrics-GUI)'}
                )
                
                with urllib.request.urlopen(req, timeout=12) as response:
                    data = json.loads(response.read().decode())
                    self._limiter.relax()
                    
                    if not data or not isinstance(data, list):
                        continue
                        
                    # Find best match based on duration if provided
                    best_match = None
                    if duration:
                        for item in data:
                            if item.get('duration') and abs(item['duration'] - duration) <= 3.0:
                                best_match = item
                                break
                    if not best_match and data:
                        best_match = data[0]
                        
                    if best_match:
                        synced = best_match.get('syncedLyrics') or ''
                        plain = best_match.get('plainLyrics') or ''
                        if synced or plain:
                            return LyricsResult(
                                synced=synced,
                                plain=plain,
                                source=f"{self.name} ({self.instance_url})",
                                confidence=0.9 if synced else 0.5
                            )
            except urllib.error.HTTPError as e:
                if e.code == 429:
                    retry_after = int(e.headers.get('Retry-After', 60))
                    self._limiter.penalize(retry_after)
                    if self._on_rate_limit:
                        self._on_rate_limit(self.name, retry_after)
                continue
            except Exception:
                continue
                
        return LyricsResult()
