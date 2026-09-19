"""Batch lyrics fetcher coordinating multi-platform downloads."""
import os
import time
import threading
from dataclasses import dataclass
from typing import Callable, Optional, List

from app.core import lrc_utils
from app.core import auditor
from app.core.scanner import TrackInfo

@dataclass
class FetchResult:
    audio_path: str
    status: str       # 'synced' | 'plain' | 'none' | 'kept' | 'error'
    source: str       # provider name
    query: str        # search query used
    error: Optional[str] = None

class BatchFetcher:
    """Fetches lyrics for a batch of tracks with progress reporting."""
    
    def __init__(self, provider_manager, db, config):
        self.manager = provider_manager
        self.db = db  
        self.config = config
        self._cancel = threading.Event()
        self._paused = threading.Event()
        self._paused.set()  # not paused initially
        self._is_running = False
        self._thread: Optional[threading.Thread] = None
    
    def start_batch(
        self,
        tracks: List[TrackInfo],
        mode: str,
        on_track_start: Callable[[int, int, TrackInfo], None],
        on_track_result: Callable[[int, int, FetchResult], None],
        on_platform_status: Callable[[TrackInfo, str, str], None],
        on_rate_limit: Callable[[str, int], None],
        on_complete: Callable[[dict], None],
    ) -> None:
        """Start batch fetching in a dedicated background daemon thread."""
        if self._is_running:
            return
            
        self._cancel.clear()
        self._paused.set()
        self._is_running = True
        
        self._thread = threading.Thread(
            target=self._run_batch,
            args=(tracks, mode, on_track_start, on_track_result, on_platform_status, on_rate_limit, on_complete),
            daemon=True
        )
        self._thread.start()

    def _run_batch(
        self,
        tracks: List[TrackInfo],
        mode: str,
        on_track_start: Callable,
        on_track_result: Callable,
        on_platform_status: Callable,
        on_rate_limit: Callable,
        on_complete: Callable,
    ) -> None:
        try:
            # Filter tracks according to mode
            filtered_tracks: List[TrackInfo] = []
            for t in tracks:
                if mode == 'missing' and t.status != 'missing':
                    continue
                elif mode == 'upgrade_plain' and t.status != 'plain':
                    continue
                elif mode == 'suspicious' and t.status != 'suspicious':
                    continue
                elif mode == 'smart' and t.status not in ('missing', 'plain', 'suspicious'):
                    continue
                # For 'replace', include all tracks
                filtered_tracks.append(t)

            total = len(filtered_tracks)
            results_summary = {'synced': 0, 'plain': 0, 'none': 0, 'kept': 0, 'error': 0}

            # Hook provider rate limit callbacks
            for p in self.manager.providers:
                p.set_rate_limit_callback(lambda name, sec: on_rate_limit(name, sec))

            for idx, track in enumerate(filtered_tracks):
                if self._cancel.is_set():
                    break
                    
                self._paused.wait()
                on_track_start(idx, total, track)

                # Check retry cache (skip if checked recently, unless mode is 'replace')
                if mode != 'replace' and self.db.is_cached(track.audio_path, self.config.retry_days):
                    res = FetchResult(track.audio_path, 'kept', '', 'Cached: checked recently')
                    results_summary['kept'] += 1
                    on_track_result(idx, total, res)
                    continue

                artist = track.artist or ""
                title = track.title or os.path.splitext(os.path.basename(track.audio_path))[0]
                duration = track.duration
                query = f"{artist} - {title}".strip(" -")
                old_text = lrc_utils.read_text(track.lrc_path) if track.status in ('plain', 'suspicious', 'synced') else None

                try:
                    # In suspicious mode, check if the current file really looks wrong
                    if mode == 'suspicious':
                        is_bad, reason = auditor.audit_track(track.audio_path, track.lrc_path)
                        if not is_bad:
                            res = FetchResult(track.audio_path, 'kept', '', query, f"Audited OK: {reason}")
                            results_summary['kept'] += 1
                            on_track_result(idx, total, res)
                            continue

                    # Search all providers sequentially
                    search_res = self.manager.search_all(
                        artist=artist,
                        title=title,
                        duration=duration,
                        on_progress_callback=lambda p_name, status: on_platform_status(track, p_name, status)
                    )

                    lrc_text = None
                    final_status = 'none'
                    source = search_res.source if search_res else ''

                    if search_res and search_res.synced:
                        # Validate synced lyrics
                        audit_err = lrc_utils.check_lrc(search_res.synced, duration, title)
                        if audit_err and search_res.plain and not lrc_utils.check_lrc(search_res.plain, duration, title):
                            lrc_text = search_res.plain
                            final_status = 'plain'
                        elif not audit_err:
                            lrc_text = search_res.synced
                            final_status = 'synced'
                    elif search_res and search_res.plain and mode != 'upgrade_plain':
                        lrc_text = search_res.plain
                        final_status = 'plain'

                    if lrc_text:
                        # Write atomically
                        lrc_utils.write_atomic(track.lrc_path, lrc_text)
                        track.status = final_status
                        track.lrc_content = lrc_text
                        
                        self.db.upsert_track(
                            track.audio_path,
                            artist=artist,
                            title=title,
                            album=track.album,
                            duration=duration,
                            lrc_path=track.lrc_path,
                            lrc_status=final_status,
                            lyrics_source=source,
                            last_checked=time.time()
                        )
                        self.db.add_history(
                            track.audio_path,
                            action="download",
                            platform=source,
                            lrc_type=final_status,
                            details=f"Downloaded {final_status} lyrics"
                        )
                        res = FetchResult(track.audio_path, final_status, source, query)
                        results_summary[final_status] += 1
                    else:
                        if mode in ('suspicious', 'upgrade_plain') and old_text:
                            # Keep existing lyrics
                            res = FetchResult(track.audio_path, 'kept', '', query, "No better version found")
                            results_summary['kept'] += 1
                        else:
                            # Not found anywhere
                            self.db.set_cache(track.audio_path)
                            self.db.add_history(
                                track.audio_path,
                                action="not_found",
                                platform="",
                                lrc_type="none",
                                details="Not found on any platform"
                            )
                            res = FetchResult(track.audio_path, 'none', '', query)
                            results_summary['none'] += 1

                    on_track_result(idx, total, res)

                except Exception as e:
                    res = FetchResult(track.audio_path, 'error', '', query, str(e))
                    results_summary['error'] += 1
                    on_track_result(idx, total, res)

                # Politeness interval between songs
                time.sleep(max(0.1, self.config.request_interval))

        finally:
            self._is_running = False
            on_complete(results_summary)

    def pause(self):
        self._paused.clear()
        
    def resume(self):
        self._paused.set()
        
    def cancel(self):
        self._cancel.set()
        self.resume()
    
    @property
    def is_running(self) -> bool:
        return self._is_running
