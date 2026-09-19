from dataclasses import dataclass, field
from abc import ABC, abstractmethod
import threading
import time
from typing import Optional, Callable
from datetime import datetime, date

@dataclass
class LyricsResult:
    synced: str = ""       # LRC text with timestamps
    plain: str = ""        # Plain text lyrics
    source: str = ""       # Provider name
    confidence: float = 0.0 # 0.0 - 1.0

@dataclass
class RateLimitConfig:
    requests_per_minute: int = 60
    requests_per_day: int = 0      # 0 = unlimited
    base_interval: float = 1.0     # seconds between requests
    max_backoff: float = 60.0      # max backoff on 429
    retry_count: int = 3

class AdaptiveRateLimiter:
    """Thread-safe adaptive rate limiter with per-minute and per-day tracking."""
    def __init__(self, config: RateLimitConfig):
        self.config = config
        self.lock = threading.Lock()
        
        self.current_interval = config.base_interval
        self.next_slot = time.time()
        
        self.minute_requests = []
        
        self.day_count = 0
        self.current_day = date.today()

    def wait(self) -> None:
        with self.lock:
            now = time.time()
            self._cleanup_minute_tracking(now)
            
            # Check daily limit
            if self.config.requests_per_day > 0:
                if self.day_count >= self.config.requests_per_day:
                    raise Exception("Daily rate limit exceeded")
            
            # Check minute limit and wait if necessary
            if len(self.minute_requests) >= self.config.requests_per_minute:
                sleep_time = self.minute_requests[0] + 60.0 - now
                if sleep_time > 0:
                    time.sleep(sleep_time)
                now = time.time()
                self._cleanup_minute_tracking(now)

            # Check interval
            if now < self.next_slot:
                time.sleep(self.next_slot - now)
            
            # Register request
            req_time = time.time()
            self.minute_requests.append(req_time)
            self.day_count += 1
            self.next_slot = req_time + self.current_interval

    def penalize(self, retry_after: Optional[float] = None) -> float:
        with self.lock:
            if retry_after is not None:
                self.current_interval = min(self.config.max_backoff, max(self.current_interval * 2, retry_after))
            else:
                self.current_interval = min(self.config.max_backoff, self.current_interval * 2)
            self.next_slot = time.time() + self.current_interval
            return self.current_interval

    def relax(self) -> None:
        with self.lock:
            self.current_interval = max(self.config.base_interval, self.current_interval * 0.9)

    def is_daily_exhausted(self) -> bool:
        with self.lock:
            self._check_day_reset()
            if self.config.requests_per_day == 0:
                return False
            return self.day_count >= self.config.requests_per_day

    def get_retry_after(self) -> float:
        with self.lock:
            now = time.time()
            return max(0.0, self.next_slot - now)
            
    def _cleanup_minute_tracking(self, now: float) -> None:
        self._check_day_reset()
        self.minute_requests = [t for t in self.minute_requests if now - t < 60.0]

    def _check_day_reset(self) -> None:
        today = date.today()
        if today != self.current_day:
            self.current_day = today
            self.day_count = 0

class BaseProvider(ABC):
    """Abstract lyrics provider with built-in rate limiting."""
    name: str = "unknown"
    can_provide_synced: bool = True
    requires_api_key: bool = False
    
    def __init__(self):
        self._limiter = AdaptiveRateLimiter(self.rate_config)
        self._on_rate_limit: Optional[Callable] = None
    
    @property
    @abstractmethod
    def rate_config(self) -> RateLimitConfig:
        pass

    @abstractmethod
    def search_lyrics(self, artist: str, title: str, duration: float | None = None) -> LyricsResult:
        """Search for lyrics. Must handle rate limiting internally."""
        ...
    
    def set_api_key(self, key: str) -> None:
        """Set API key if provider supports it."""
        pass
    
    def set_rate_limit_callback(self, callback: Callable[[str, int], None]) -> None:
        """Set callback(provider_name, retry_after_seconds) for rate limit notifications."""
        self._on_rate_limit = callback
    
    @property
    def is_available(self) -> bool:
        """False if daily limit exhausted."""
        return not self._limiter.is_daily_exhausted()
    
    @property 
    def daily_remaining(self) -> int | None:
        """Remaining daily requests, or None if unlimited."""
        if self.rate_config.requests_per_day == 0:
            return None
        return max(0, self.rate_config.requests_per_day - self._limiter.day_count)
