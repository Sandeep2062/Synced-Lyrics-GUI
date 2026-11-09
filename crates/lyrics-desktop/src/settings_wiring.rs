use crate::MainWindow;
use lyrics_core::{save_settings, AppSettings};
use slint::{ComponentHandle, Timer};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Updates and links
// ---------------------------------------------------------------------------

pub(crate) const REPOSITORY_URL: &str = "https://github.com/Sandeep2062/Synced-Lyrics-GUI";
pub(crate) const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/Sandeep2062/Synced-Lyrics-GUI/releases/latest";

/// One line describing whether a newer release exists. Never fails: an offline
/// machine or a repository with no releases yet gets a sentence, not an error.
pub(crate) fn latest_release_status() -> String {
    let current = env!("CARGO_PKG_VERSION");
    let client = match reqwest::blocking::Client::builder()
        .user_agent(concat!("SyncedLyrics/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(e) => return format!("Could not check for updates: {e}"),
    };

    let response = match client.get(LATEST_RELEASE_API).send() {
        Ok(response) => response,
        Err(e) => return format!("Could not reach GitHub: {e}"),
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return "No releases published yet".to_string();
    }
    if !response.status().is_success() {
        return format!("GitHub returned {} — try again later", response.status());
    }

    let body: serde_json::Value = match response.json() {
        Ok(body) => body,
        Err(e) => return format!("Unexpected reply from GitHub: {e}"),
    };
    let Some(tag) = body.get("tag_name").and_then(|tag| tag.as_str()) else {
        return "No releases published yet".to_string();
    };

    if version_is_newer(tag.trim_start_matches(['v', 'V']), current) {
        format!("Version {tag} is available")
    } else {
        format!("Up to date (v{current})")
    }
}

/// Dotted-numeric comparison. Anything it cannot parse (a pre-release suffix,
/// say) counts as "not newer", so it can never nag about a tag it does not
/// understand.
pub(crate) fn version_is_newer(candidate: &str, current: &str) -> bool {
    fn numbers(version: &str) -> Option<Vec<u32>> {
        version
            .split('.')
            .map(|part| part.trim().parse().ok())
            .collect()
    }
    let (Some(candidate), Some(current)) = (numbers(candidate), numbers(current)) else {
        return false;
    };
    for index in 0..candidate.len().max(current.len()) {
        let new = candidate.get(index).copied().unwrap_or(0);
        let old = current.get(index).copied().unwrap_or(0);
        if new != old {
            return new > old;
        }
    }
    false
}

/// Hands `url` to the desktop's default browser. `std` has no "open a link",
/// but every platform has a one-liner for it.
pub(crate) fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };

    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn apply_settings_to_ui(window: &MainWindow, settings: &AppSettings) {
    window.set_app_version(env!("CARGO_PKG_VERSION").into());
    window.set_settings_enable_lrclib(settings.lrclib_enabled);
    window.set_settings_enable_musixmatch(settings.musixmatch_enabled);
    window.set_settings_musixmatch_key(settings.musixmatch_api_key.clone().into());
    window.set_settings_enable_netease(settings.netease_enabled);
    window.set_settings_enable_megalobiz(settings.megalobiz_enabled);
    window.set_settings_enable_genius(settings.genius_enabled);
    window.set_settings_genius_token(settings.genius_api_token.clone().into());
    window.set_settings_lrclib_url(settings.lrclib_base_url.clone().into());
    window.set_settings_request_interval(settings.request_interval_secs as f32);
    window.set_volume(settings.volume);
    let cache_idx = match settings.retry_days {
        0..=7 => 0,
        8..=14 => 1,
        15..=30 => 2,
        _ => 3,
    };
    window.set_settings_cache_period_idx(cache_idx);
    window.set_settings_scan_on_startup(settings.scan_on_startup);
    window.set_settings_backup_before_overwrite(settings.backup_lyrics_before_overwrite);
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

pub(crate) fn wire_settings_and_updates(window: &MainWindow) {
    {
        let weak = window.as_weak();
        window.on_save_settings(move || {
            let Some(w) = weak.upgrade() else { return };
            let settings = AppSettings {
                lrclib_enabled: w.get_settings_enable_lrclib(),
                musixmatch_enabled: w.get_settings_enable_musixmatch(),
                musixmatch_api_key: w.get_settings_musixmatch_key().to_string(),
                netease_enabled: w.get_settings_enable_netease(),
                megalobiz_enabled: w.get_settings_enable_megalobiz(),
                genius_enabled: w.get_settings_enable_genius(),
                genius_api_token: w.get_settings_genius_token().to_string(),
                lrclib_base_url: w.get_settings_lrclib_url().to_string(),
                request_interval_secs: w.get_settings_request_interval() as f64,
                retry_days: match w.get_settings_cache_period_idx() {
                    0 => 7,
                    1 => 14,
                    2 => 30,
                    _ => 0,
                },
                volume: w.get_volume(),
                window_width: 1280,
                window_height: 820,
                scan_on_startup: w.get_settings_scan_on_startup(),
                backup_lyrics_before_overwrite: w.get_settings_backup_before_overwrite(),
            };
            let status = match save_settings(&settings) {
                Ok(()) => "Saved".to_string(),
                Err(e) => format!("Could not save: {e}"),
            };
            w.set_settings_status(status.into());
            let weak = w.as_weak();
            Timer::single_shot(Duration::from_secs(2), move || {
                if let Some(w) = weak.upgrade() {
                    w.set_settings_status("".into());
                }
            });
        });
    }
    {
        window.on_open_github(move || {
            if let Err(e) = open_in_browser(REPOSITORY_URL) {
                eprintln!("Could not open {REPOSITORY_URL}: {e}");
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_check_updates(move || {
            let Some(w) = weak.upgrade() else { return };
            w.set_updates_checking(true);
            w.set_updates_status("Checking for updates…".into());
            let weak = w.as_weak();
            thread::spawn(move || {
                let status = latest_release_status();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_updates_checking(false);
                        w.set_updates_status(status.into());
                    }
                });
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_handles_the_shapes_a_release_tag_takes() {
        assert!(version_is_newer("1.0.1", "1.0.0"));
        assert!(version_is_newer("0.2", "0.1.9"));
        assert!(version_is_newer("2.0.0", "1.9.9"));
        assert!(!version_is_newer("1.0.0", "1.0.0"));
        assert!(!version_is_newer("0.9.9", "1.0.0"));
        // A tag we cannot parse must never report a phantom update.
        assert!(!version_is_newer("1.1.0-beta.1", "1.0.0"));
        assert!(!version_is_newer("", "1.0.0"));
    }
}
