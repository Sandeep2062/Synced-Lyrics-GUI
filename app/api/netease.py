from .base import BaseProvider, LyricsResult, RateLimitConfig
import re

try:
    from syncedlyrics.providers import NetEase as SLNetEase
except ImportError:
    SLNetEase = None

class NetEaseProvider(BaseProvider):
    name = 'NetEase'
    can_provide_synced = True
    requires_api_key = False
    
    @property
    def rate_config(self) -> RateLimitConfig:
        return RateLimitConfig(requests_per_minute=40, requests_per_day=0, base_interval=1.5)

    def search_lyrics(self, artist: str, title: str, duration: float | None = None) -> LyricsResult:
        if not SLNetEase:
            return LyricsResult()
            
        self._limiter.wait()
        
        try:
            provider = SLNetEase()
            query = f"{title} {artist}"
            lrc = provider.get_lrc(query)
            self._limiter.relax()
            
            if lrc:
                plain = re.sub(r'\[.*?\]', '', lrc).strip()
                return LyricsResult(
                    synced=lrc,
                    plain=plain,
                    source=self.name,
                    confidence=0.8
                )
        except Exception as e:
            if '429' in str(e):
                self._limiter.penalize()
                if self._on_rate_limit:
                    self._on_rate_limit(self.name, 60)
                    
        return LyricsResult()
