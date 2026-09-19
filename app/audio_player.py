import time
import pygame

class AudioPlayer:
    """Simple audio player with position tracking."""
    
    def __init__(self):
        pygame.mixer.init()
        self._is_playing = False
        self._start_time = 0.0
        self._pause_time = 0.0
        self._seek_offset = 0.0
        self._duration = 0.0

    def load(self, path: str) -> bool:
        try:
            pygame.mixer.music.load(path)
            self._duration = 0.0 # Will be updated manually if needed outside
            self._seek_offset = 0.0
            return True
        except Exception:
            return False

    def play(self):
        pygame.mixer.music.play()
        self._is_playing = True
        self._start_time = time.time() - self._seek_offset
        self._pause_time = 0.0

    def pause(self):
        if self._is_playing:
            pygame.mixer.music.pause()
            self._is_playing = False
            self._pause_time = time.time()

    def resume(self):
        if not self._is_playing and self._pause_time > 0:
            pygame.mixer.music.unpause()
            self._is_playing = True
            # Adjust start time so position is correct
            paused_duration = time.time() - self._pause_time
            self._start_time += paused_duration

    def stop(self):
        pygame.mixer.music.stop()
        self._is_playing = False
        self._seek_offset = 0.0
        self._start_time = 0.0
        self._pause_time = 0.0

    def seek(self, position_seconds: float):
        self._seek_offset = position_seconds
        if self._is_playing:
            pygame.mixer.music.set_pos(position_seconds)
            self._start_time = time.time() - self._seek_offset
        elif self._pause_time > 0:
            self._pause_time = time.time()
            self._start_time = time.time() - self._seek_offset

    def set_volume(self, volume: float):
        pygame.mixer.music.set_volume(max(0.0, min(1.0, volume)))
    
    @property
    def is_playing(self) -> bool:
        return self._is_playing
        
    @property
    def position(self) -> float:
        if self._is_playing:
            return time.time() - self._start_time
        elif self._pause_time > 0:
            return self._pause_time - self._start_time
        return self._seek_offset
        
    @property
    def duration(self) -> float:
        return self._duration
        
    def cleanup(self):
        self.stop()
        pygame.mixer.quit()
