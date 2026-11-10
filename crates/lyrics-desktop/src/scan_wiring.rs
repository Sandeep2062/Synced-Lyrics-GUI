use crate::art_loader::ArtLoader;
use crate::library_model::{load_library_into_ui, open_database, AppState};
use crate::MainWindow;
use lyrics_core::{scan_directory_incremental, MutexExt};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Background Startup Scan
// ---------------------------------------------------------------------------

pub(crate) fn spawn_startup_scan(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
) {
    let event_loop = window.as_weak();
    let state = Arc::clone(app_state);
    let cache = Arc::clone(art_loader);
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

// ---------------------------------------------------------------------------
// Directory Management & Scan Wiring
// ---------------------------------------------------------------------------

pub(crate) fn wire_add_directory(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
) {
    let weak = window.as_weak();
    let state = Arc::clone(app_state);
    let cache = Arc::clone(art_loader);
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
                    art_loader.reset_after_scan();
                    load_library_into_ui(&w, &state, &art_loader);
                }
            });
        });
    });
}

pub(crate) fn wire_refresh(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
) {
    let weak = window.as_weak();
    let state = Arc::clone(app_state);
    let cache = Arc::clone(art_loader);
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

                let mut all_paths = Vec::new();
                for dir in &dirs {
                    let mut dir_paths = Vec::new();
                    if lyrics_core::collect_audio_paths(std::path::Path::new(dir), &mut dir_paths)
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
                            let mut last = last_update_cb.lock_ignore_poison();
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

pub(crate) fn wire_directory_dialog(window: &MainWindow) {
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
}

pub(crate) fn wire_scan_and_directories(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
) {
    wire_add_directory(window, app_state, art_loader);
    wire_refresh(window, app_state, art_loader);
    wire_directory_dialog(window);
}
