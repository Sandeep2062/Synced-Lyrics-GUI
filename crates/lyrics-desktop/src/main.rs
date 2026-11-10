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
mod settings_wiring;
pub(crate) use settings_wiring::*;
mod scan_wiring;
pub(crate) use scan_wiring::*;

use lyrics_core::{load_settings, AudioEngine, LrcLine};
use slint::{Model, ModelRc, VecModel};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};



// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<(), slint::PlatformError> {
    lyrics_core::setup_panic_hook();
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
    if let Ok(mut eng) = audio.lock() {
        if settings.muted {
            eng.set_volume(0.0);
        } else {
            eng.set_volume(settings.volume);
        }
    }

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
        spawn_startup_scan(&window, &app_state, &art_loader);
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

    wire_scan_and_directories(&window, &app_state, &art_loader);

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
    // Settings and Updates
    // -----------------------------------------------------------------------
    wire_settings_and_updates(&window);


    window.run()
}
#[cfg(test)]
mod ui_audit;

#[cfg(test)]
pub(crate) mod ui_snapshot;

#[cfg(test)]
mod headless_tests;

