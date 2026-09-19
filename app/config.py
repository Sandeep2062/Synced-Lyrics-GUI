"""Configuration manager."""
import json
import threading
from pathlib import Path
from typing import Any, Dict, List

from app.constants import CONFIG_FILE

class Config:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._config: Dict[str, Any] = {
            "directories": [],
            "api_keys": {
                "musixmatch": "",
                "genius": ""
            },
            "workers": 2,
            "request_interval": 0.8,
            "retry_days": 14,
            "window_geometry": "1280x800",
            "last_view": "library",
            "volume": 0.7,
            "theme": "dark"
        }
        self.load()

    def load(self) -> None:
        with self._lock:
            if CONFIG_FILE.exists():
                try:
                    with open(CONFIG_FILE, "r", encoding="utf-8") as f:
                        data = json.load(f)
                        self._update_nested_dict(self._config, data)
                except Exception as e:
                    print(f"Error loading config: {e}")

    def _update_nested_dict(self, d: Dict[str, Any], u: Dict[str, Any]) -> None:
        for k, v in u.items():
            if isinstance(v, dict) and k in d and isinstance(d[k], dict):
                self._update_nested_dict(d[k], v)
            else:
                d[k] = v

    def save(self) -> None:
        with self._lock:
            try:
                with open(CONFIG_FILE, "w", encoding="utf-8") as f:
                    json.dump(self._config, f, indent=4)
            except Exception as e:
                print(f"Error saving config: {e}")

    @property
    def directories(self) -> List[str]:
        with self._lock:
            return list(self._config["directories"])

    @property
    def api_keys(self) -> Dict[str, str]:
        with self._lock:
            return dict(self._config["api_keys"])

    @property
    def workers(self) -> int:
        with self._lock:
            return self._config["workers"]

    @workers.setter
    def workers(self, val: int) -> None:
        with self._lock:
            self._config["workers"] = val

    @property
    def request_interval(self) -> float:
        with self._lock:
            return self._config["request_interval"]

    @request_interval.setter
    def request_interval(self, val: float) -> None:
        with self._lock:
            self._config["request_interval"] = val

    @property
    def retry_days(self) -> int:
        with self._lock:
            return self._config["retry_days"]

    @retry_days.setter
    def retry_days(self, val: int) -> None:
        with self._lock:
            self._config["retry_days"] = val

    @property
    def window_geometry(self) -> str:
        with self._lock:
            return self._config["window_geometry"]

    @window_geometry.setter
    def window_geometry(self, val: str) -> None:
        with self._lock:
            self._config["window_geometry"] = val

    @property
    def last_view(self) -> str:
        with self._lock:
            return self._config["last_view"]

    @last_view.setter
    def last_view(self, val: str) -> None:
        with self._lock:
            self._config["last_view"] = val
            
    @property
    def volume(self) -> float:
        with self._lock:
            return self._config["volume"]

    @volume.setter
    def volume(self, val: float) -> None:
        with self._lock:
            self._config["volume"] = val
            
    @property
    def theme(self) -> str:
        with self._lock:
            return self._config["theme"]

    @theme.setter
    def theme(self, val: str) -> None:
        with self._lock:
            self._config["theme"] = val

    def add_directory(self, path: str) -> None:
        with self._lock:
            if path not in self._config["directories"]:
                self._config["directories"].append(path)
        self.save()

    def remove_directory(self, path: str) -> None:
        with self._lock:
            if path in self._config["directories"]:
                self._config["directories"].remove(path)
        self.save()

    def get_api_key(self, provider: str) -> str:
        with self._lock:
            return self._config["api_keys"].get(provider, "")

    def set_api_key(self, provider: str, key: str) -> None:
        with self._lock:
            self._config["api_keys"][provider] = key
        self.save()

_config_instance = None
_config_lock = threading.Lock()

def get_config() -> Config:
    global _config_instance
    with _config_lock:
        if _config_instance is None:
            _config_instance = Config()
        return _config_instance
