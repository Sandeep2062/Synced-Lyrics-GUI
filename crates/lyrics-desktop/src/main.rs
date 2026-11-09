#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod audio_source;
mod art_loader;
pub(crate) use art_loader::*;
mod library_model;
pub(crate) use library_model::*;
mod playback;
pub(crate) use playback::*;
mod lyrics_sync;
pub(crate) use lyrics_sync::*;
mod downloads;
pub(crate) use downloads::*;
mod health_wiring;
pub(crate) use health_wiring::*;

use lyrics_core::{
    load_settings, save_settings, scan_directory_incremental,
    AppSettings, LrcLine,
};

use slint::{Model, ModelRc, SharedString, Timer, VecModel};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;



// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<(), slint::PlatformError> {
    let window = MainWindow::new()?;
    window.show()?;
    #[allow(clippy::arc_with_non_send_sync)]
    let audio = Arc::new(Mutex::new(RodioAudioEngine::new()));
    let lyric_state = Arc::new(Mutex::new(Vec::<LrcLine>::new()));
    let app_state = Arc::new(Mutex::new(AppState::new()));
    let art_loader = Arc::new(ArtLoader::new());
    art_loader.spawn_workers(window.as_weak());
    let waveform_cache: WaveformCacheMap = Arc::new(Mutex::new(HashMap::new()));
    let batch_job_control: Arc<Mutex<Option<ActiveBatchJob>>> = Arc::new(Mutex::new(None));
    let health_job_control: Arc<Mutex<Option<ActiveBatchJob>>> = Arc::new(Mutex::new(None));

    // Set initial default waveform in UI
    window.set_waveform_data(ModelRc::new(VecModel::from(default_waveform())));

    // Load settings into UI
    let settings = load_settings().unwrap_or_default();
    apply_settings_to_ui(&window, &settings);

    // Load saved library immediately
    load_library_into_ui(&window, &app_state, &art_loader);

    // Nothing can be done in an empty library, so on a first run (or after every
    // folder has been removed) the directory picker opens itself. It closes as
    // soon as a folder is scanned.
    if window.get_settings_music_dirs().row_count() == 0 {
        window.set_directories_open(true);
    }

    // Automatically scan library in background on open if enabled in settings
    if settings.scan_on_startup {
        let event_loop = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        thread::spawn(move || {
            let _ = (|| -> Result<(), String> {
                let db = open_database()?;
                let dirs = db.directories().map_err(|e| e.to_string())?;
                if dirs.is_empty() {
                    return Ok(());
                }
                let existing_map = db.get_tracks_map().map_err(|e| e.to_string())?;
                let mut any_changed = false;
                for dir in &dirs {
                    if let Ok(res) = scan_directory_incremental(dir, Some(&existing_map)) {
                        if !res.changed_tracks.is_empty() {
                            let _ = db.upsert_tracks(&res.changed_tracks);
                            any_changed = true;
                        }
                    }
                }
                if any_changed {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = event_loop.upgrade() {
                            load_library_into_ui(&w, &state, &cache);
                        }
                    });
                }
                Ok(())
            })();
        });
    }

    // -----------------------------------------------------------------------
    // Playback engine, waveforms, controls & timer
    // -----------------------------------------------------------------------
    let _position_timer = wire_playback(
        &window,
        &app_state,
        &audio,
        &lyric_state,
        &art_loader,
        &waveform_cache,
    );

    // -----------------------------------------------------------------------
    // Add a music folder
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        window.on_add_directory(move || {
            let Some(folder) = rfd::FileDialog::new()
                .set_title("Choose music folder")
                .pick_folder()
            else {
                return;
            };
            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let result = (|| -> Result<(), String> {
                    let db = open_database()?;
                    db.add_directory(folder.to_string_lossy().as_ref())
                        .map_err(|e| e.to_string())?;
                    let existing_map = db.get_tracks_map().map_err(|e| e.to_string())?;
                    let res = scan_directory_incremental(&folder, Some(&existing_map))
                        .map_err(|e| format!("{e:?}"))?;
                    if !res.changed_tracks.is_empty() {
                        db.upsert_tracks(&res.changed_tracks)
                            .map_err(|e| e.to_string())?;
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    eprintln!("Scan error: {e}");
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = event_loop.upgrade() {
                        // A folder was just scanned: covers may have appeared
                        // next to files, and files may have been re-tagged, so
                        // nothing the artwork index remembers is trustworthy.
                        art_loader.reset_after_scan();
                        load_library_into_ui(&w, &state, &art_loader);
                    }
                });
            });
        });
    }

    // -----------------------------------------------------------------------
    // Refresh
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        window.on_refresh(move || {
            let Some(w) = weak.upgrade() else { return };
            if w.get_is_refreshing() {
                return;
            }
            w.set_is_refreshing(true);
            w.set_refresh_progress(0.0);
            w.set_refresh_text(SharedString::from("Refreshing..."));

            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let result = (|| -> Result<(), String> {
                    let db = open_database()?;
                    let dirs = db.directories().map_err(|e| e.to_string())?;
                    let existing_map = db.get_tracks_map().map_err(|e| e.to_string())?;

                    // Efficiently discover paths without repeating directory walks
                    let mut all_paths = Vec::new();
                    for dir in &dirs {
                        let mut dir_paths = Vec::new();
                        if lyrics_core::collect_audio_paths(
                            std::path::Path::new(dir),
                            &mut dir_paths,
                        )
                        .is_ok()
                        {
                            dir_paths.sort();
                            all_paths.extend(dir_paths);
                        }
                    }

                    let total_files = all_paths.len();
                    if total_files == 0 {
                        return Ok(());
                    }

                    let last_update = Arc::new(Mutex::new(std::time::Instant::now()));
                    let event_loop_cb = event_loop.clone();
                    let last_update_cb = Arc::clone(&last_update);

                    let res = lyrics_core::scan_audio_paths_incremental(
                        all_paths,
                        Some(&existing_map),
                        move |idx, total| {
                            let should_update = {
                                let mut last = last_update_cb.lock().unwrap();
                                if idx == 0
                                    || idx == total
                                    || last.elapsed() >= Duration::from_millis(50)
                                {
                                    *last = std::time::Instant::now();
                                    true
                                } else {
                                    false
                                }
                            };

                            if should_update {
                                let prog = if total > 0 {
                                    idx as f32 / total as f32
                                } else {
                                    1.0
                                };
                                let pct = (prog * 100.0).round() as u32;
                                let el = event_loop_cb.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(w) = el.upgrade() {
                                        w.set_refresh_progress(prog);
                                        w.set_refresh_text(SharedString::from(format!(
                                            "Refreshing... {pct}%"
                                        )));
                                    }
                                });
                            }
                        },
                    );

                    if !res.changed_tracks.is_empty() {
                        let _ = db.upsert_tracks(&res.changed_tracks);
                    }
                    Ok(())
                })();

                if let Err(e) = result {
                    eprintln!("Refresh scan error: {e}");
                }

                let el = event_loop.clone();
                let st = Arc::clone(&state);
                let ac = Arc::clone(&art_loader);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el.upgrade() {
                        art_loader.reset_after_scan();
                        load_library_into_ui(&w, &st, &ac);
                        w.set_refresh_progress(1.0);
                        w.set_refresh_text(SharedString::from("Done!"));
                    }
                });

                // Keep completion state briefly visible for feedback
                thread::sleep(Duration::from_millis(700));
                let el_reset = event_loop.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el_reset.upgrade() {
                        w.set_is_refreshing(false);
                        w.set_refresh_progress(0.0);
                        w.set_refresh_text(SharedString::from("Refresh Directory"));
                    }
                });
            });
        });
    }

    // -----------------------------------------------------------------------
    // Batch Download & Track Fetch / Identification
    // -----------------------------------------------------------------------
    wire_downloads(
        &window,
        &app_state,
        &audio,
        &art_loader,
        &batch_job_control,
        &lyric_state,
    );

    // -----------------------------------------------------------------------
    // Library Health View: Audits, safe fixes, preview diff, result logs & crash resumption
    // -----------------------------------------------------------------------
    wire_health_audit(&window, &app_state, &art_loader, &health_job_control);

    // -----------------------------------------------------------------------
    // Search and filter
    // -----------------------------------------------------------------------
    wire_search_and_filters(&window, &app_state, &art_loader);


    // -----------------------------------------------------------------------
    // Lyrics: synchronization, offsets, clipboard & fixing
    // -----------------------------------------------------------------------
    wire_lyrics_sync(
        &window,
        &app_state,
        &audio,
        &lyric_state,
        &art_loader,
        &waveform_cache,
    );

    // -----------------------------------------------------------------------
    // Albums and Artists views
    // -----------------------------------------------------------------------
    wire_album_and_artist_views(&window, &app_state, &art_loader);


    // -----------------------------------------------------------------------
    // Settings
    // -----------------------------------------------------------------------
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
            // Clear the confirmation on its own, so it reads as feedback rather
            // than as a label that is always there.
            let weak = w.as_weak();
            Timer::single_shot(Duration::from_secs(2), move || {
                if let Some(w) = weak.upgrade() {
                    w.set_settings_status("".into());
                }
            });
        });
    }

    // -----------------------------------------------------------------------
    // About: open the repository, and look for a newer release
    // -----------------------------------------------------------------------
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

    // -----------------------------------------------------------------------
    // Music directories
    //
    // These live in their own dialog rather than on the settings screen: they
    // describe the library, not a preference, and on a first run they are the
    // only thing that can be done.
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        window.on_manage_directories(move || {
            if let Some(w) = weak.upgrade() {
                w.set_directories_open(true);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_close_directories(move || {
            if let Some(w) = weak.upgrade() {
                w.set_directories_open(false);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_remove_directory(move |dir_idx| {
            let _ = (|| -> Result<(), String> {
                let db = open_database()?;
                let dirs = db.directories().map_err(|e| e.to_string())?;
                if let Some(path) = dirs.get(dir_idx as usize) {
                    db.remove_directory(path).map_err(|e| e.to_string())?;
                }
                let remaining = db.directories().map_err(|e| e.to_string())?;
                if let Some(w) = weak.upgrade() {
                    w.set_settings_music_dirs(ModelRc::new(VecModel::from(
                        remaining
                            .into_iter()
                            .map(SharedString::from)
                            .collect::<Vec<_>>(),
                    )));
                }
                Ok(())
            })();
        });
    }

    window.run()
}

// ---------------------------------------------------------------------------
// Updates and links
// ---------------------------------------------------------------------------

const REPOSITORY_URL: &str = "https://github.com/Sandeep2062/Synced-Lyrics-GUI";
const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/Sandeep2062/Synced-Lyrics-GUI/releases/latest";

/// One line describing whether a newer release exists. Never fails: an offline
/// machine or a repository with no releases yet gets a sentence, not an error.
fn latest_release_status() -> String {
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
fn version_is_newer(candidate: &str, current: &str) -> bool {
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
fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        // `start` is a cmd builtin; the empty argument is the window title it
        // would otherwise take the URL for.
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

fn apply_settings_to_ui(window: &MainWindow, settings: &AppSettings) {
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

#[cfg(test)]
mod ui_audit;

#[cfg(test)]
mod ui_snapshot;

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
