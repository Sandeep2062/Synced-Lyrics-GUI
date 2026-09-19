from .base import BaseProvider, LyricsResult, RateLimitConfig
import re

try:
    from syncedlyrics.providers import Megalobiz as SLMegalobiz
except ImportError:
    SLMegalobiz = None

class MegalobizProvider(BaseProvider):
    name = 'Megalobiz'
    can_provide_synced = True
    requires_api_key = False
    
    @property
    def rate_config(self) -> RateLimitConfig:
        return RateLimitConfig(requests_per_minute=30, requests_per_day=0, base_interval=2.0)

    def search_lyrics(self, artist: str, title: str, duration: float | None = None) -> LyricsResult:
        if not SLMegalobiz:
            return LyricsResult()
            
        self._limiter.wait()
        
        try:
            provider = SLMegalobiz()
            query = f"{title} {artist}"
            lrc = provider.get_lrc(query)
            self._limiter.relax()
            
            if lrc:
                plain = re.sub(r'\[.*?\]', '', lrc).strip()
                return LyricsResult(
                    synced=lrc,
                    plain=plain,
                    source=self.name,
                    confidence=0.7
                )
        except Exception as e:
            if '429' in str(e):
                self._limiter.penalize()
                if self._on_rate_limit:
                    self._on_rate_limit(self.name, 60)
                    
        return LyricsResult()
