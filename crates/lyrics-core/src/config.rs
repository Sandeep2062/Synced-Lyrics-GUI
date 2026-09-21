use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine the user home directory")]
    MissingHome,
    #[error("could not read settings: {0}")]
    Read(#[source] std::io::Error),
    #[error("could not write settings: {0}")]
    Write(#[source] std::io::Error),
    #[error("could not parse settings: {0}")]
    Parse(#[source] serde_json::Error),
    #[error("could not serialize settings: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("could not access the OS credential store: {0}")]
    Credential(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub lrclib_enabled: bool,
    pub musixmatch_enabled: bool,
    pub musixmatch_api_key: String,
    pub netease_enabled: bool,
    pub megalobiz_enabled: bool,
    pub genius_enabled: bool,
    pub genius_api_token: String,
    pub lrclib_base_url: String,
    pub request_interval_secs: f64,
    pub retry_days: u32,
    pub volume: f32,
    pub window_width: u32,
    pub window_height: u32,
    pub scan_on_startup: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            lrclib_enabled: true,
            musixmatch_enabled: true,
            musixmatch_api_key: String::new(),
            netease_enabled: true,
            megalobiz_enabled: true,
            genius_enabled: true,
            genius_api_token: String::new(),
            lrclib_base_url: "https://lrclib.net".to_string(),
            request_interval_secs: 0.8,
            retry_days: 14,
            volume: 0.7,
            window_width: 1280,
            window_height: 820,
            scan_on_startup: false,
        }
    }
}

#[derive(Debug, Deserialize)]
struct LegacyConfig {
    #[serde(default)]
    api_keys: HashMap<String, String>,
    #[serde(default)]
    request_interval: Option<f64>,
    #[serde(default)]
    retry_days: Option<u32>,
    #[serde(default)]
    volume: Option<f32>,
    #[serde(default)]
    window_geometry: Option<String>,
}

pub fn app_data_dir() -> Result<PathBuf, ConfigError> {
    // 1. Explicit override environment variable (useful for testing & custom setups)
    if let Some(override_dir) = env::var_os("SYNCED_LYRICS_DATA_DIR") {
        return Ok(PathBuf::from(override_dir));
    }

    // 2. Portable mode: if portable marker or data directory exists next to executable
    if let Ok(exe_path) = env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let portable_dir = exe_dir.join("data");
            let portable_dat = exe_dir.join("portable.dat");
            let portable_txt = exe_dir.join("portable.txt");

            if env::var_os("SYNCED_LYRICS_PORTABLE").is_some()
                || portable_dir.is_dir()
                || portable_dat.is_file()
                || portable_txt.is_file()
            {
                return Ok(portable_dir);
            }
        }
    }

    // 3. Standard system application data directories
    #[cfg(target_os = "windows")]
    if let Some(app_data) = env::var_os("APPDATA") {
        return Ok(PathBuf::from(app_data).join("SyncedLyrics"));
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = home_dir() {
        return Ok(home
            .join("Library")
            .join("Application Support")
            .join("SyncedLyrics"));
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(data_home).join("synced-lyrics"));
    }

    home_dir()
        .map(|home| home.join(".local").join("share").join("synced-lyrics"))
        .ok_or(ConfigError::MissingHome)
}

/// Returns the legacy application data directory from previous Python releases.
pub fn legacy_app_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    if let Some(app_data) = env::var_os("APPDATA") {
        return Some(PathBuf::from(app_data).join("SyncedLyricsGUI"));
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = home_dir() {
        return Some(home.join(".SyncedLyricsGUI"));
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    if let Some(data_home) = env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(data_home).join("SyncedLyricsGUI"));
    }

    home_dir().map(|home| home.join(".SyncedLyricsGUI"))
}

pub fn settings_path() -> Result<PathBuf, ConfigError> {
    Ok(app_data_dir()?.join("settings.json"))
}

pub fn legacy_settings_path() -> Option<PathBuf> {
    legacy_app_data_dir().map(|dir| dir.join("config.json"))
}

pub fn parse_legacy_config_str(contents: &str) -> Result<AppSettings, ConfigError> {
    let legacy: LegacyConfig = serde_json::from_str(contents).map_err(ConfigError::Parse)?;
    let mut settings = AppSettings::default();

    if let Some(key) = legacy.api_keys.get("musixmatch") {
        if !key.trim().is_empty() {
            settings.musixmatch_api_key = key.trim().to_string();
        }
    }
    if let Some(token) = legacy.api_keys.get("genius") {
        if !token.trim().is_empty() {
            settings.genius_api_token = token.trim().to_string();
        }
    }
    if let Some(interval) = legacy.request_interval {
        if interval > 0.0 {
            settings.request_interval_secs = interval;
        }
    }
    if let Some(days) = legacy.retry_days {
        settings.retry_days = days;
    }
    if let Some(vol) = legacy.volume {
        if (0.0..=1.0).contains(&vol) {
            settings.volume = vol;
        }
    }
    if let Some(geom) = legacy.window_geometry {
        if let Some((w, h)) = geom.split_once('x') {
            if let (Ok(width), Ok(height)) = (w.parse::<u32>(), h.parse::<u32>()) {
                if width >= 400 && height >= 300 {
                    settings.window_width = width;
                    settings.window_height = height;
                }
            }
        }
    }

    Ok(settings)
}

pub fn load_settings() -> Result<AppSettings, ConfigError> {
    let path = settings_path()?;
    let mut settings = if path.exists() {
        let contents = fs::read_to_string(path).map_err(ConfigError::Read)?;
        let mut loaded: AppSettings =
            serde_json::from_str(&contents).map_err(ConfigError::Parse)?;
        // If an existing configuration has all secondary providers disabled
        // (the legacy default from older builds without toggle support),
        // automatically enable the zero-config providers.
        if !loaded.musixmatch_enabled
            && !loaded.netease_enabled
            && !loaded.megalobiz_enabled
            && !loaded.genius_enabled
        {
            loaded.musixmatch_enabled = true;
            loaded.netease_enabled = true;
            loaded.megalobiz_enabled = true;
            loaded.genius_enabled = true;
        }
        loaded
    } else if let Some(legacy_path) = legacy_settings_path() {
        if legacy_path.exists() {
            match fs::read_to_string(&legacy_path) {
                Ok(contents) => match parse_legacy_config_str(&contents) {
                    Ok(migrated) => {
                        let _ = save_settings(&migrated);
                        migrated
                    }
                    Err(_) => AppSettings::default(),
                },
                Err(_) => AppSettings::default(),
            }
        } else {
            AppSettings::default()
        }
    } else {
        AppSettings::default()
    };
    settings.musixmatch_api_key = load_credential("musixmatch-api-key").unwrap_or_default();
    settings.genius_api_token = load_credential("genius-api-token").unwrap_or_default();
    Ok(settings)
}

pub fn save_settings(settings: &AppSettings) -> Result<(), ConfigError> {
    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ConfigError::Write)?;
    }
    save_credential("musixmatch-api-key", &settings.musixmatch_api_key)?;
    save_credential("genius-api-token", &settings.genius_api_token)?;
    let mut persisted = settings.clone();
    persisted.musixmatch_api_key.clear();
    persisted.genius_api_token.clear();
    let contents = serde_json::to_string_pretty(&persisted).map_err(ConfigError::Serialize)?;
    fs::write(path, contents).map_err(ConfigError::Write)
}

fn credential_entry(name: &str) -> Result<keyring::Entry, ConfigError> {
    keyring::Entry::new("synced-lyrics-gui", name)
        .map_err(|error| ConfigError::Credential(error.to_string()))
}

fn load_credential(name: &str) -> Result<String, ConfigError> {
    match credential_entry(name)?.get_password() {
        Ok(value) => Ok(value),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(error) => Err(ConfigError::Credential(error.to_string())),
    }
}

fn save_credential(name: &str, value: &str) -> Result<(), ConfigError> {
    let entry = credential_entry(name)?;
    if value.trim().is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(ConfigError::Credential(error.to_string())),
        }
    } else {
        entry
            .set_password(value)
            .map_err(|error| ConfigError::Credential(error.to_string()))
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_all_providers_enabled() {
        let settings = AppSettings::default();
        assert!(settings.lrclib_enabled);
        assert!(settings.musixmatch_enabled);
        assert!(settings.netease_enabled);
        assert!(settings.megalobiz_enabled);
        assert!(settings.genius_enabled);
    }

    #[test]
    fn accepts_partial_json_with_defaults() {
        let settings: AppSettings = serde_json::from_str(r#"{"lrclib_enabled":false}"#).unwrap();
        assert!(!settings.lrclib_enabled);
        assert!(settings.musixmatch_enabled);
        assert!(settings.netease_enabled);
        assert!(settings.megalobiz_enabled);
        assert!(settings.genius_enabled);
    }

    #[test]
    fn migrates_legacy_secondary_disabled_config() {
        let json = r#"{
            "lrclib_enabled": true,
            "musixmatch_enabled": false,
            "netease_enabled": false,
            "megalobiz_enabled": false,
            "genius_enabled": false
        }"#;
        let mut loaded: AppSettings = serde_json::from_str(json).unwrap();
        if !loaded.musixmatch_enabled
            && !loaded.netease_enabled
            && !loaded.megalobiz_enabled
            && !loaded.genius_enabled
        {
            loaded.musixmatch_enabled = true;
            loaded.netease_enabled = true;
            loaded.megalobiz_enabled = true;
            loaded.genius_enabled = true;
        }
        assert!(loaded.musixmatch_enabled);
        assert!(loaded.netease_enabled);
        assert!(loaded.megalobiz_enabled);
        assert!(loaded.genius_enabled);
    }

    #[test]
    fn parses_legacy_python_config_fields() {
        let legacy_json = r#"{
            "directories": ["C:\\Music", "D:\\Songs"],
            "api_keys": {
                "musixmatch": "mx_secret_123",
                "genius": "gn_secret_456"
            },
            "workers": 4,
            "request_interval": 1.5,
            "retry_days": 30,
            "window_geometry": "1600x900",
            "volume": 0.85
        }"#;
        let settings = parse_legacy_config_str(legacy_json).expect("failed to parse legacy config");
        assert_eq!(settings.musixmatch_api_key, "mx_secret_123");
        assert_eq!(settings.genius_api_token, "gn_secret_456");
        assert!((settings.request_interval_secs - 1.5).abs() < f64::EPSILON);
        assert_eq!(settings.retry_days, 30);
        assert_eq!(settings.window_width, 1600);
        assert_eq!(settings.window_height, 900);
        assert!((settings.volume - 0.85).abs() < 1e-4);
    }

    #[test]
    fn app_data_dir_respects_custom_override() {
        let custom_dir = std::env::temp_dir().join("synced_lyrics_custom_test_dir");
        std::env::set_var("SYNCED_LYRICS_DATA_DIR", &custom_dir);
        let path = app_data_dir().expect("app_data_dir should succeed");
        std::env::remove_var("SYNCED_LYRICS_DATA_DIR");
        assert_eq!(path, custom_dir);
    }
}
