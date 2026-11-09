use crate::art_loader::ArtLoader;
use crate::library_model::{current_timestamp, load_library_into_ui, open_database, AppState};
use crate::playback::RodioAudioEngine;
use crate::{BatchLogEntry, BatchStats, MainWindow};
use lyrics_core::{
    download_track, load_settings, AppSettings, DownloadEvent, DownloadMode, DownloadOutcome,
    GeniusProvider, IdentificationSuggestion, LibraryDb, LrcLine, LrclibProvider, LyricsProvider,
    LyricsStatus, MegalobizProvider, MusixmatchProvider, NeteaseProvider, ProviderLyrics,
    ProviderName, Track,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Active batch job handle & cached lyrics provider
// ---------------------------------------------------------------------------

pub(crate) struct ActiveBatchJob {
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) paused: Arc<AtomicBool>,
}

pub(crate) struct CachedLyricsProvider {
    lyrics: ProviderLyrics,
}

impl LyricsProvider for CachedLyricsProvider {
    fn name(&self) -> ProviderName {
        self.lyrics.provider
    }
    fn search(
        &self,
        track: &Track,
    ) -> Result<Vec<lyrics_core::Candidate>, lyrics_core::ProviderError> {
        Ok(vec![lyrics_core::Candidate {
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: (!track.album.trim().is_empty()).then(|| track.album.clone()),
            duration_seconds: track.duration_seconds,
            synced: self.lyrics.synced.clone(),
            plain: self.lyrics.plain.clone(),
            provider: self.lyrics.provider,
        }])
    }
}

pub(crate) fn providers_for_settings(
    database: &LibraryDb,
    track: &Track,
    settings: &AppSettings,
) -> Result<Vec<Box<dyn LyricsProvider>>, String> {
    let mut providers: Vec<Box<dyn LyricsProvider>> = Vec::new();
    let ttl_seconds = match settings.retry_days {
        0 => 0_u64,
        d => d as u64 * 86400,
    };

    macro_rules! add_provider {
        ($enabled:expr, $name:expr, $cached_name:expr, $ctor:expr) => {
            if $enabled {
                if let Some(lyrics) = database
                    .cached_lyrics(&track.audio_path, $cached_name)
                    .map_err(|e| e.to_string())?
                {
                    providers.push(Box::new(CachedLyricsProvider { lyrics }));
                } else {
                    providers.push(Box::new(
                        $ctor.map_err(|e: lyrics_core::ProviderError| e.to_string())?,
                    ));
                }
            }
        };
    }

    add_provider!(
        settings.lrclib_enabled,
        "LRCLib",
        ProviderName::Lrclib,
        LrclibProvider::new(&settings.lrclib_base_url)
    );
    add_provider!(
        settings.musixmatch_enabled,
        "Musixmatch",
        ProviderName::Musixmatch,
        MusixmatchProvider::new(settings.musixmatch_api_key.clone())
    );
    add_provider!(
        settings.netease_enabled,
        "NetEase",
        ProviderName::Netease,
        NeteaseProvider::new()
    );
    add_provider!(
        settings.megalobiz_enabled,
        "Megalobiz",
        ProviderName::Megalobiz,
        MegalobizProvider::new()
    );
    add_provider!(
        settings.genius_enabled,
        "Genius",
        ProviderName::Genius,
        GeniusProvider::new(settings.genius_api_token.clone())
    );

    let _ = ttl_seconds;
    Ok(providers)
}

// ---------------------------------------------------------------------------
// Single Track Fetch & Replace Helpers (<100 lines each)
// ---------------------------------------------------------------------------

pub(crate) fn fetch_and_save_track_lyrics(
    track: &Track,
    mode: DownloadMode,
) -> Result<Option<LyricsStatus>, String> {
    let db = open_database()?;
    let settings = load_settings().map_err(|e| e.to_string())?;
    let providers = providers_for_settings(&db, track, &settings)?;
    let cancelled = AtomicBool::new(false);
    let outcome = download_track(track, mode, &providers, &cancelled, |_| {})
        .map_err(|e| e.to_string())?;

    if let DownloadOutcome::Downloaded { provider, synced } = outcome {
        let new_status = if synced {
            LyricsStatus::Synced
        } else {
            LyricsStatus::Plain
        };
        db.update_lyrics_status(&track.audio_path, new_status, Some(provider.label()))
            .map_err(|e| e.to_string())?;
        Ok(Some(new_status))
    } else {
        Ok(None)
    }
}

pub(crate) fn apply_accepted_identification(
    track: &mut Track,
    suggestion: &IdentificationSuggestion,
) -> Result<(), String> {
    lyrics_core::apply_tag_suggestion(&track.audio_path, suggestion)
        .map_err(|e| format!("Lofty tag write error: {e}"))?;

    let db = open_database()?;
    let album_to_store = suggestion
        .suggested_album
        .as_deref()
        .unwrap_or(&track.album);
    db.update_track_metadata(
        &track.audio_path,
        &suggestion.suggested_artist,
        &suggestion.suggested_title,
        album_to_store,
    )
    .map_err(|e| e.to_string())?;

    track.artist = suggestion.suggested_artist.clone();
    track.title = suggestion.suggested_title.clone();
    if let Some(ref alb) = suggestion.suggested_album {
        track.album = alb.clone();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Batch Download Execution Helpers (<100 lines each)
// ---------------------------------------------------------------------------

fn rescan_batch_directories(db: &LibraryDb, cancelled: &Arc<AtomicBool>) -> Vec<Track> {
    let dirs = db.directories().unwrap_or_default();
    let existing_map = db.get_tracks_map().unwrap_or_default();
    let mut all_paths = Vec::new();
    for dir in &dirs {
        let mut dir_paths = Vec::new();
        if lyrics_core::collect_audio_paths(std::path::Path::new(dir), &mut dir_paths).is_ok() {
            dir_paths.sort();
            all_paths.extend(dir_paths);
        }
    }
    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
        return Vec::new();
    }
    let scan_res = lyrics_core::scan_audio_paths_incremental(all_paths, Some(&existing_map), |_, _| {});
    if !scan_res.changed_tracks.is_empty() {
        let _ = db.upsert_tracks(&scan_res.changed_tracks);
    }
    db.tracks().unwrap_or_default()
}

fn download_single_batch_track(
    db: &LibraryDb,
    track: &Track,
    mode: DownloadMode,
    settings: &AppSettings,
    cancelled: &Arc<AtomicBool>,
    stats: &mut BatchStats,
    was_cancelled: &mut bool,
) -> BatchLogEntry {
    let provider_results = Arc::new(Mutex::new(Vec::<(ProviderName, Option<bool>, Option<String>)>::new()));
    let outcome_res = match providers_for_settings(db, track, settings) {
        Ok(providers) => {
            let pr_cb = Arc::clone(&provider_results);
            download_track(track, mode, &providers, cancelled, move |event| {
                if let Ok(mut pr) = pr_cb.lock() {
                    match event {
                        DownloadEvent::ProviderFound { provider, synced } => {
                            pr.push((provider, Some(synced), None));
                        }
                        DownloadEvent::ProviderNotFound { provider } => {
                            pr.push((provider, None, None));
                        }
                        DownloadEvent::ProviderFailed { provider, message } => {
                            pr.push((provider, None, Some(message)));
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|e| e.to_string())
        }
        Err(e) => Err(e),
    };

    let pr_list = provider_results.lock().map(|l| l.clone()).unwrap_or_default();
    let mut parts = Vec::new();
    for (p, status, err) in pr_list {
        if let Some(synced) = status {
            parts.push(format!("✓ {} ({})", p.label(), if synced { "synced" } else { "plain" }));
        } else if err.is_some() {
            parts.push(format!("⚠ {} (error)", p.label()));
        } else {
            parts.push(format!("✗ {}", p.label()));
        }
    }
    let providers_summary = if parts.is_empty() {
        "No provider responses".to_string()
    } else {
        parts.join("  |  ")
    };

    let (badge_text, badge_type, result_action) = match outcome_res {
        Ok(DownloadOutcome::Downloaded { provider, synced }) => {
            if synced {
                stats.synced += 1;
                let _ = db.update_lyrics_status(&track.audio_path, LyricsStatus::Synced, Some(provider.label()));
                ("SYNCED", "synced", format!("→ Saved synced lyrics from {}", provider.label()))
            } else {
                stats.plain += 1;
                let _ = db.update_lyrics_status(&track.audio_path, LyricsStatus::Plain, Some(provider.label()));
                ("PLAIN", "plain", format!("→ Saved plain lyrics from {}", provider.label()))
            }
        }
        Ok(DownloadOutcome::NotFound) => {
            stats.not_found += 1;
            ("NOT FOUND", "missing", "→ No matching lyrics found".to_string())
        }
        Ok(DownloadOutcome::Skipped) => {
            ("SKIPPED", "info", "→ Skipped (kept existing lyrics)".to_string())
        }
        Ok(DownloadOutcome::Cancelled) => {
            *was_cancelled = true;
            ("CANCELLED", "missing", "→ Cancelled by user".to_string())
        }
        Ok(DownloadOutcome::DryRun { .. }) => {
            ("DRY RUN", "info", "→ Dry run (no lyrics written)".to_string())
        }
        Err(err) => {
            stats.errors += 1;
            ("ERROR", "error", format!("→ Error: {err}"))
        }
    };

    BatchLogEntry {
        timestamp: current_timestamp().into(),
        track_title: track.title.clone().into(),
        track_artist: track.artist.clone().into(),
        details: providers_summary.into(),
        action_text: result_action.into(),
        badge_text: badge_text.into(),
        badge_type: badge_type.into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_batch_download_loop(
    db: &LibraryDb,
    candidates: &[Track],
    mode: DownloadMode,
    settings: &AppSettings,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
    event_loop: &slint::Weak<MainWindow>,
    state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
    logs: &mut Vec<BatchLogEntry>,
) -> (BatchStats, bool) {
    let total_candidates = candidates.len();
    let mut stats = BatchStats {
        total: total_candidates as i32,
        processed: 0,
        remaining: total_candidates as i32,
        synced: 0,
        plain: 0,
        not_found: 0,
        errors: 0,
    };
    let mut was_cancelled = false;

    for (i, track) in candidates.iter().enumerate() {
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            was_cancelled = true;
            break;
        }
        while paused.load(std::sync::atomic::Ordering::Relaxed)
            && !cancelled.load(std::sync::atomic::Ordering::Relaxed)
        {
            thread::sleep(Duration::from_millis(150));
        }
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            was_cancelled = true;
            break;
        }

        let cur_display = format!("{} — {}", track.artist, track.title);
        let prog = i as f32 / total_candidates as f32;
        let prog_text = format!("Song {} of {} ({} remaining)", i + 1, total_candidates, total_candidates - i);

        let el = event_loop.clone();
        let cd_clone = cur_display.clone();
        let pt_clone = prog_text.clone();
        let stats_clone = stats.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = el.upgrade() {
                w.set_batch_current_track(cd_clone.into());
                w.set_batch_progress(prog);
                w.set_batch_progress_text(pt_clone.into());
                w.set_batch_stats(stats_clone);
            }
        });

        let entry = download_single_batch_track(
            db,
            track,
            mode,
            settings,
            cancelled,
            &mut stats,
            &mut was_cancelled,
        );
        stats.processed = (i + 1) as i32;
        stats.remaining = (total_candidates - (i + 1)) as i32;
        logs.push(entry);

        let el = event_loop.clone();
        let logs_clone = logs.clone();
        let stats_clone = stats.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = el.upgrade() {
                w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
                w.set_batch_stats(stats_clone);
            }
        });

        if (i + 1) % 5 == 0 || i + 1 == total_candidates {
            let el = event_loop.clone();
            let st = Arc::clone(state);
            let ac = Arc::clone(art_loader);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = el.upgrade() {
                    load_library_into_ui(&w, &st, &ac);
                }
            });
        }

        if was_cancelled {
            break;
        }
    }

    (stats, was_cancelled)
}

#[allow(clippy::too_many_arguments)]
fn finish_batch_worker(
    event_loop: &slint::Weak<MainWindow>,
    state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
    job_ctrl: &Arc<Mutex<Option<ActiveBatchJob>>>,
    logs: Vec<BatchLogEntry>,
    stats: BatchStats,
    total_candidates: usize,
    was_cancelled: bool,
) {
    if let Ok(mut lock) = job_ctrl.lock() {
        *lock = None;
    }

    let el = event_loop.clone();
    let st = Arc::clone(state);
    let ac = Arc::clone(art_loader);
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(w) = el.upgrade() {
            w.set_batch_logs(ModelRc::new(VecModel::from(logs)));
            w.set_batch_stats(stats.clone());
            w.set_batch_progress(1.0);
            w.set_batch_progress_text(format!("{}/{}", stats.processed, total_candidates).into());
            w.set_batch_current_track(
                if was_cancelled { "Batch download stopped." } else { "Batch download completed!" }.into(),
            );
            w.set_batch_phase_text(
                format!(
                    "Finished: {} synced, {} plain, {} not found, {} errors",
                    stats.synced, stats.plain, stats.not_found, stats.errors
                )
                .into(),
            );
            w.set_batch_is_running(false);
            w.set_batch_is_finished(true);
            load_library_into_ui(&w, &st, &ac);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn run_batch_download_worker(
    event_loop: slint::Weak<MainWindow>,
    state: Arc<Mutex<AppState>>,
    art_loader: Arc<ArtLoader>,
    job_ctrl: Arc<Mutex<Option<ActiveBatchJob>>>,
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    mode: DownloadMode,
    mut logs: Vec<BatchLogEntry>,
) {
    let db = match open_database() {
        Ok(db) => db,
        Err(e) => {
            logs.push(BatchLogEntry {
                timestamp: current_timestamp().into(),
                track_title: "Database Error".into(),
                track_artist: "System".into(),
                details: format!("Could not open database: {e}").into(),
                action_text: "".into(),
                badge_text: "ERROR".into(),
                badge_type: "error".into(),
            });
            let el = event_loop.clone();
            let logs_clone = logs.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = el.upgrade() {
                    w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
                    w.set_batch_is_running(false);
                    w.set_batch_is_finished(true);
                    w.set_batch_phase_text("Error opening database".into());
                }
            });
            return;
        }
    };

    let all_tracks = rescan_batch_directories(&db, &cancelled);
    let el = event_loop.clone();
    let st = Arc::clone(&state);
    let ac = Arc::clone(&art_loader);
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(w) = el.upgrade() {
            ac.reset_after_scan();
            load_library_into_ui(&w, &st, &ac);
        }
    });

    logs.push(BatchLogEntry {
        timestamp: current_timestamp().into(),
        track_title: "Rescan Complete".into(),
        track_artist: "Library".into(),
        details: format!("Discovered {} total tracks in library.", all_tracks.len()).into(),
        action_text: "".into(),
        badge_text: "SCAN".into(),
        badge_type: "scan".into(),
    });

    let el = event_loop.clone();
    let logs_clone = logs.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(w) = el.upgrade() {
            w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
        }
    });

    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }

    let candidates: Vec<Track> = all_tracks.into_iter().filter(|t| t.should_download(mode)).collect();
    let total_candidates = candidates.len();
    if total_candidates == 0 {
        logs.push(BatchLogEntry {
            timestamp: current_timestamp().into(),
            track_title: "No Matching Songs".into(),
            track_artist: "Batch".into(),
            details: "No tracks matching this download criteria were found in your library.".into(),
            action_text: "".into(),
            badge_text: "INFO".into(),
            badge_type: "info".into(),
        });
        let el = event_loop.clone();
        let logs_clone = logs.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = el.upgrade() {
                w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
                w.set_batch_progress(1.0);
                w.set_batch_progress_text("0 / 0".into());
                w.set_batch_phase_text("Completed: No matching tracks found".into());
                w.set_batch_current_track("No tracks to download".into());
                w.set_batch_is_running(false);
                w.set_batch_is_finished(true);
            }
        });
        return;
    }

    let settings = load_settings().unwrap_or_default();
    let el = event_loop.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(w) = el.upgrade() {
            w.set_batch_phase_text(format!("Phase 2: Downloading lyrics ({} tracks)", total_candidates).into());
        }
    });

    let (stats, was_cancelled) = execute_batch_download_loop(
        &db,
        &candidates,
        mode,
        &settings,
        &cancelled,
        &paused,
        &event_loop,
        &state,
        &art_loader,
        &mut logs,
    );

    logs.push(BatchLogEntry {
        timestamp: current_timestamp().into(),
        track_title: if was_cancelled { "Process Stopped" } else { "Process Finished" }.into(),
        track_artist: "Batch".into(),
        details: format!(
            "Processed {}/{} tracks. Synced: {}, Plain: {}, Not Found: {}, Errors: {}",
            stats.processed, total_candidates, stats.synced, stats.plain, stats.not_found, stats.errors
        )
        .into(),
        action_text: "".into(),
        badge_text: if was_cancelled { "STOPPED" } else { "DONE" }.into(),
        badge_type: if was_cancelled { "missing" } else { "synced" }.into(),
    });

    finish_batch_worker(
        &event_loop,
        &state,
        &art_loader,
        &job_ctrl,
        logs,
        stats,
        total_candidates,
        was_cancelled,
    );
}

// ---------------------------------------------------------------------------
// Wiring Helpers (<150 lines each)
// ---------------------------------------------------------------------------

pub(crate) fn wire_batch_dialog(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
    job_ctrl: &Arc<Mutex<Option<ActiveBatchJob>>>,
) {
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        window.on_download_all(move || {
            let Some(w) = weak.upgrade() else { return };
            if w.get_batch_is_running()
                || (w.get_batch_is_finished() && w.get_batch_download_stage() == 1)
            {
                w.set_batch_download_open(true);
                return;
            }
            let Ok(s) = state.lock() else { return };
            let total = s.all_tracks.len() as i32;
            let missing = s.all_tracks.iter().filter(|t| t.status == LyricsStatus::Missing).count() as i32;
            let plain = s.all_tracks.iter().filter(|t| t.status == LyricsStatus::Plain).count() as i32;
            let suspicious = s.all_tracks.iter().filter(|t| t.status == LyricsStatus::Suspicious).count() as i32;
            drop(s);

            w.set_batch_total_count(total);
            w.set_batch_missing_count(missing);
            w.set_batch_plain_count(plain);
            w.set_batch_suspicious_count(suspicious);
            w.set_batch_download_stage(0);
            w.set_batch_download_open(true);
        });
    }
    {
        let weak = window.as_weak();
        window.on_batch_close(move || {
            if let Some(w) = weak.upgrade() {
                w.set_batch_download_open(false);
            }
        });
    }
    {
        let job = Arc::clone(job_ctrl);
        let weak = window.as_weak();
        window.on_batch_pause_resume(move || {
            if let Ok(lock) = job.lock() {
                if let Some(ref handle) = *lock {
                    let was_paused = handle.paused.load(std::sync::atomic::Ordering::Relaxed);
                    handle.paused.store(!was_paused, std::sync::atomic::Ordering::Relaxed);
                    if let Some(w) = weak.upgrade() {
                        w.set_batch_is_paused(!was_paused);
                    }
                }
            }
        });
    }
    {
        let job = Arc::clone(job_ctrl);
        let weak = window.as_weak();
        window.on_batch_stop(move || {
            if let Ok(mut lock) = job.lock() {
                if let Some(ref handle) = *lock {
                    handle.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                    if let Some(w) = weak.upgrade() {
                        w.set_batch_is_running(false);
                        w.set_batch_is_finished(true);
                        w.set_batch_phase_text("Stopping process...".into());
                    }
                }
                *lock = None;
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let cache = Arc::clone(art_loader);
        let job_ctrl = Arc::clone(job_ctrl);
        window.on_batch_start_mode(move |mode_idx| {
            let Some(w) = weak.upgrade() else { return };
            let (mode, mode_title) = match mode_idx {
                0 => (DownloadMode::ReplaceAll, "Batch Download: Whole Directory"),
                1 => (DownloadMode::MissingOnly, "Batch Download: Missing Only"),
                2 => (DownloadMode::UpgradePlain, "Batch Download: Upgrade Plain to Synced"),
                3 => (DownloadMode::FixSuspicious, "Batch Download: Fix Suspicious"),
                _ => (DownloadMode::MissingOnly, "Batch Download"),
            };

            let cancelled = Arc::new(AtomicBool::new(false));
            let paused = Arc::new(AtomicBool::new(false));
            if let Ok(mut lock) = job_ctrl.lock() {
                *lock = Some(ActiveBatchJob {
                    cancelled: Arc::clone(&cancelled),
                    paused: Arc::clone(&paused),
                });
            }

            w.set_batch_mode_title(SharedString::from(mode_title));
            w.set_batch_phase_text(SharedString::from("Phase 1: Rescanning music directories..."));
            w.set_batch_current_track(SharedString::from("Rescanning music directories..."));
            w.set_batch_progress(0.0);
            w.set_batch_progress_text(SharedString::from("Rescanning..."));
            w.set_batch_stats(BatchStats {
                total: 0,
                processed: 0,
                remaining: 0,
                synced: 0,
                plain: 0,
                not_found: 0,
                errors: 0,
            });

            let initial_logs = vec![BatchLogEntry {
                timestamp: current_timestamp().into(),
                track_title: "Rescan Directories".into(),
                track_artist: "Library".into(),
                details: "Rescanning music folders to update tracks before starting download...".into(),
                action_text: "".into(),
                badge_text: "SCAN".into(),
                badge_type: "scan".into(),
            }];
            w.set_batch_logs(ModelRc::new(VecModel::from(initial_logs.clone())));

            w.set_batch_download_stage(1);
            w.set_batch_is_running(true);
            w.set_batch_is_paused(false);
            w.set_batch_is_finished(false);

            let event_loop = w.as_weak();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            let job_ctrl = Arc::clone(&job_ctrl);
            thread::spawn(move || {
                run_batch_download_worker(
                    event_loop,
                    state,
                    art_loader,
                    job_ctrl,
                    cancelled,
                    paused,
                    mode,
                    initial_logs,
                );
            });
        });
    }
}

pub(crate) fn wire_single_track_downloads(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    art_loader: &Arc<ArtLoader>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
) {
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let cache = Arc::clone(art_loader);
        window.on_fetch_track(move |row_idx| {
            let Ok(s) = state.lock() else { return };
            let row = row_idx as usize;
            let Some(&track_idx) = s.filtered_tracks.get(row) else { return };
            let Some(track) = s.all_tracks.get(track_idx).cloned() else { return };
            drop(s);
            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let _ = fetch_and_save_track_lyrics(&track, DownloadMode::ReplaceAll);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = event_loop.upgrade() {
                        load_library_into_ui(&w, &state, &art_loader);
                    }
                });
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let engine = Arc::clone(audio);
        let cache = Arc::clone(art_loader);
        window.on_fetch_current_lyrics(move || {
            let Ok(eng) = engine.lock() else { return };
            let Some(current_path) = eng.current_path.clone() else { return };
            drop(eng);
            let Ok(s) = state.lock() else { return };
            let Some(track) = s.all_tracks.iter().find(|t| t.audio_path == current_path).cloned() else { return };
            drop(s);
            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let _ = fetch_and_save_track_lyrics(&track, DownloadMode::ReplaceAll);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = event_loop.upgrade() {
                        load_library_into_ui(&w, &state, &art_loader);
                    }
                });
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let audio_eng = Arc::clone(audio);
        let lyric_st = Arc::clone(lyric_state);
        let cache = Arc::clone(art_loader);
        window.on_replace_lyrics(move || {
            let current_path = audio_eng.lock().ok().and_then(|e| e.current_path.clone());
            let Ok(s) = state.lock() else { return };
            let track_opt = if let Some(ref path) = current_path {
                s.all_tracks.iter().find(|t| &t.audio_path == path).cloned()
            } else if let Some(row) = s.current_filtered_row {
                s.filtered_tracks.get(row).and_then(|&idx| s.all_tracks.get(idx)).cloned()
            } else {
                None
            };
            let Some(track) = track_opt else { return };
            drop(s);

            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            let lyric_state = Arc::clone(&lyric_st);
            thread::spawn(move || {
                let _ = (|| -> Result<(), String> {
                    let db = open_database()?;
                    let settings = load_settings().map_err(|e| e.to_string())?;
                    let providers = providers_for_settings(&db, &track, &settings)?;
                    let cancelled = AtomicBool::new(false);
                    let outcome = download_track(&track, DownloadMode::ReplaceAll, &providers, &cancelled, |_| {})
                        .map_err(|e| e.to_string())?;
                    if let DownloadOutcome::Downloaded { provider, synced } = outcome {
                        db.update_lyrics_status(
                            &track.audio_path,
                            if synced { LyricsStatus::Synced } else { LyricsStatus::Plain },
                            Some(provider.label()),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    let track_lrc = track.lrc_path.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = event_loop.upgrade() {
                            load_library_into_ui(&w, &state, &art_loader);
                            if let Ok(contents) = std::fs::read_to_string(&track_lrc) {
                                let lines = lyrics_core::parse_lines(&contents);
                                if !lines.is_empty() {
                                    let ui_lines: Vec<crate::LyricLine> = lines
                                        .iter()
                                        .map(|l| crate::LyricLine {
                                            timestamp: l.timestamp_seconds as f32,
                                            text: l.text.clone().into(),
                                        })
                                        .collect();
                                    w.set_lyrics_lines(ModelRc::new(VecModel::from(ui_lines)));
                                    w.set_lyrics_plain_text("".into());
                                    w.set_lyrics_status("synced".into());
                                    w.set_lyrics_is_suspicious(false);
                                    w.set_lyrics_suspicious_reason("".into());
                                    w.set_lyrics_has_offset(false);
                                    if let Ok(mut ls) = lyric_state.lock() {
                                        *ls = lines;
                                    }
                                }
                            }
                        }
                    });
                    Ok(())
                })();
            });
        });
    }
}

fn wire_identify_track_actions(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
) {
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        window.on_identify_track(move |row_idx| {
            let Ok(s) = state.lock() else { return };
            let row = row_idx as usize;
            let Some(&track_idx) = s.filtered_tracks.get(row) else { return };
            let Some(track) = s.all_tracks.get(track_idx).cloned() else { return };
            drop(s);
            let event_loop = weak.clone();
            thread::spawn(move || {
                let settings = load_settings().unwrap_or_default();
                let lrclib_url = if settings.lrclib_enabled {
                    Some(settings.lrclib_base_url.as_str())
                } else {
                    None
                };
                let suggestion = lyrics_core::identify_track(&track, lrclib_url);
                let _ = slint::invoke_from_event_loop(move || {
                    if let (Some(w), Some(sug)) = (event_loop.upgrade(), suggestion) {
                        w.set_lyrics_suggested_artist(sug.suggested_artist.into());
                        w.set_lyrics_suggested_title(sug.suggested_title.into());
                        w.set_lyrics_suggested_album(sug.suggested_album.unwrap_or_default().into());
                        w.set_lyrics_has_identification_suggestion(true);
                    }
                });
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let engine = Arc::clone(audio);
        window.on_identify_current_track(move || {
            let Ok(eng) = engine.lock() else { return };
            let Some(current_path) = eng.current_path.clone() else { return };
            drop(eng);
            let Ok(s) = state.lock() else { return };
            let Some(track) = s.all_tracks.iter().find(|t| t.audio_path == current_path).cloned() else { return };
            drop(s);
            let event_loop = weak.clone();
            if let Some(w) = event_loop.upgrade() {
                w.set_lyrics_is_identifying(true);
            }
            thread::spawn(move || {
                let settings = load_settings().unwrap_or_default();
                let lrclib_url = if settings.lrclib_enabled {
                    Some(settings.lrclib_base_url.as_str())
                } else {
                    None
                };
                let suggestion = lyrics_core::identify_track(&track, lrclib_url);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = event_loop.upgrade() {
                        w.set_lyrics_is_identifying(false);
                        if let Some(sug) = suggestion {
                            w.set_lyrics_suggested_artist(sug.suggested_artist.into());
                            w.set_lyrics_suggested_title(sug.suggested_title.into());
                            w.set_lyrics_suggested_album(sug.suggested_album.unwrap_or_default().into());
                            w.set_lyrics_has_identification_suggestion(true);
                        }
                    }
                });
            });
        });
    }
}

fn wire_accept_and_ignore_identification(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    art_loader: &Arc<ArtLoader>,
) {
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let audio_eng = Arc::clone(audio);
        let cache = Arc::clone(art_loader);
        window.on_accept_current_identification(move || {
            let Some(w) = weak.upgrade() else { return };
            let current_path = audio_eng.lock().ok().and_then(|e| e.current_path.clone());
            let Ok(s) = state.lock() else { return };
            let track_opt = if let Some(ref path) = current_path {
                s.all_tracks.iter().find(|t| &t.audio_path == path).cloned()
            } else if let Some(row) = s.current_filtered_row {
                s.filtered_tracks.get(row).and_then(|&idx| s.all_tracks.get(idx)).cloned()
            } else {
                None
            };
            let Some(mut track) = track_opt else { return };
            drop(s);

            let suggested_artist = w.get_lyrics_suggested_artist().to_string();
            let suggested_title = w.get_lyrics_suggested_title().to_string();
            let suggested_album = w.get_lyrics_suggested_album().to_string();
            w.set_lyrics_has_identification_suggestion(false);

            let suggestion = lyrics_core::IdentificationSuggestion {
                current_artist: track.artist.clone(),
                current_title: track.title.clone(),
                current_album: track.album.clone(),
                suggested_artist: suggested_artist.clone(),
                suggested_title: suggested_title.clone(),
                suggested_album: if suggested_album.trim().is_empty() { None } else { Some(suggested_album) },
                confidence: 1.0,
                source: "user_accepted".to_string(),
                reason: "Accepted by user".to_string(),
            };

            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let _ = (|| -> Result<(), String> {
                    apply_accepted_identification(&mut track, &suggestion)?;
                    if let Ok(mut s) = state.lock() {
                        if let Some(t) = s.all_tracks.iter_mut().find(|t| t.audio_path == track.audio_path) {
                            t.artist = track.artist.clone();
                            t.title = track.title.clone();
                            t.album = track.album.clone();
                        }
                    }
                    let updated_track = track.clone();
                    let el1 = event_loop.clone();
                    let st1 = Arc::clone(&state);
                    let al1 = Arc::clone(&art_loader);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = el1.upgrade() {
                            w.set_track_title(updated_track.title.clone().into());
                            w.set_track_subtitle(format!("{} • {}", updated_track.artist, updated_track.album).into());
                            load_library_into_ui(&w, &st1, &al1);
                        }
                    });
                    let _ = fetch_and_save_track_lyrics(&track, DownloadMode::ReplaceAll);
                    let el2 = event_loop.clone();
                    let st2 = Arc::clone(&state);
                    let al2 = Arc::clone(&art_loader);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = el2.upgrade() {
                            load_library_into_ui(&w, &st2, &al2);
                        }
                    });
                    Ok(())
                })();
            });
        });
    }
    {
        let weak = window.as_weak();
        window.on_ignore_current_identification(move || {
            if let Some(w) = weak.upgrade() {
                w.set_lyrics_has_identification_suggestion(false);
            }
        });
    }
}

pub(crate) fn wire_track_identification(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    art_loader: &Arc<ArtLoader>,
) {
    wire_identify_track_actions(window, app_state, audio);
    wire_accept_and_ignore_identification(window, app_state, audio, art_loader);
}

pub(crate) fn wire_downloads(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    art_loader: &Arc<ArtLoader>,
    job_ctrl: &Arc<Mutex<Option<ActiveBatchJob>>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
) {
    wire_batch_dialog(window, app_state, art_loader, job_ctrl);
    wire_single_track_downloads(window, app_state, audio, art_loader, lyric_state);
    wire_track_identification(window, app_state, audio, art_loader);
}
