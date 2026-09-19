from typing import Callable, Optional, Dict, List
from .base import BaseProvider, LyricsResult
from .lrclib import LRCLibProvider
from .musixmatch import MusixmatchProvider
from .netease import NetEaseProvider
from .megalobiz import MegalobizProvider
from .genius import GeniusProvider

class ProviderManager:
    def __init__(self):
        self.providers: List[BaseProvider] = [
            LRCLibProvider(),
            MusixmatchProvider(),
            NetEaseProvider(),
            MegalobizProvider(),
            GeniusProvider()
        ]
        
    def configure_api_keys(self, keys: dict) -> None:
        """Distribute API keys to providers that support them."""
        for provider in self.providers:
            if provider.name.lower() in keys:
                provider.set_api_key(keys[provider.name.lower()])

    def search_all(self, artist: str, title: str, duration: Optional[float] = None, on_progress_callback: Optional[Callable[[str, str], None]] = None) -> LyricsResult:
        """
        Queries each provider sequentially.
        Calls on_progress(provider_name, status)
        """
        best_plain = None
        
        for provider in self.providers:
            if not provider.is_available:
                if on_progress_callback:
                    on_progress_callback(provider.name, 'rate_limited')
                continue
                
            if on_progress_callback:
                on_progress_callback(provider.name, 'searching')
                
            try:
                result = provider.search_lyrics(artist, title, duration)
                
                if result and result.synced:
                    if on_progress_callback:
                        on_progress_callback(provider.name, 'found_synced')
                    return result
                elif result and result.plain:
                    if on_progress_callback:
                        on_progress_callback(provider.name, 'found_plain')
                    if not best_plain or result.confidence > best_plain.confidence:
                        best_plain = result
                else:
                    if on_progress_callback:
                        on_progress_callback(provider.name, 'not_found')
            except Exception:
                if on_progress_callback:
                    on_progress_callback(provider.name, 'error')
                    
        if best_plain:
            return best_plain
            
        return LyricsResult()

    def get_provider_status(self) -> Dict[str, dict]:
        status = {}
        for provider in self.providers:
            status[provider.name] = {
                'available': provider.is_available,
                'daily_remaining': provider.daily_remaining
            }
        return status

    def is_any_available(self) -> bool:
        return any(p.is_available for p in self.providers)

    def get_estimated_retry_time(self) -> Optional[int]:
        times = []
        for provider in self.providers:
            retry_after = provider._limiter.get_retry_after()
            if retry_after > 0:
                times.append(int(retry_after))
            else:
                return 0 # immediately available
        if times:
            return min(times)
        return None
