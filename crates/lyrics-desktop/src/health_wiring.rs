use crate::art_loader::ArtLoader;
use crate::downloads::{providers_for_settings, ActiveBatchJob};
use crate::library_model::{format_unix_timestamp, load_library_into_ui, open_database, AppState};
use crate::{HealthResultLog, HealthTrackRow, MainWindow, PreviewDiffItem};
use lyrics_core::{
    apply_offset_to_lrc, compute_health_summary, download_track_dry_run,
    download_track_with_options, generate_unified_diff, load_settings, parse_offset_from_reason,
    save_settings, write_atomic_with_backup, AppSettings, BatchJobItemDraft,
    BatchJobItemRecord, BatchJobRecord, DownloadMode, DownloadOptions, DownloadOutcome,
    HealthCategory, HealthIssue, LibraryDb, LyricsStatus, Track,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Health summary & filtering helpers
// ---------------------------------------------------------------------------

pub(crate) fn load_health_into_ui(window: &MainWindow, all_tracks: &[Track]) {
    let (stats, issues) = compute_health_summary(all_tracks);
    window.set_health_total_count(stats.total as i32);
    window.set_health_synced_count(stats.synced as i32);
    window.set_health_plain_count(stats.plain as i32);
    window.set_health_missing_count(stats.missing as i32);
    window.set_health_suspicious_count(stats.suspicious as i32);
    window.set_health_safe_issues_count(stats.safe_issues as i32);
    window.set_health_wrong_song_count(stats.wrong_song_count as i32);
    window.set_health_offset_count(stats.offset_count as i32);
    window.set_health_too_short_long_count(stats.too_short_long_count as i32);

    // Check for resumable batch run in SQLite
    if let Ok(db) = open_database() {
        if let Ok(Some(job)) = db.get_resumable_batch_job() {
            window.set_health_has_resumable_job(true);
            window.set_health_resumable_progress_text(
                format!(
                    "{} / {} tracks completed",
                    job.completed_tracks, job.total_tracks
                )
                .into(),
            );
        } else {
            window.set_health_has_resumable_job(false);
            window.set_health_resumable_progress_text("".into());
        }

        // Load latest result logs (showing provider, score, why chosen)
        if let Ok(results) = db.get_latest_batch_results(60) {
            let log_rows: Vec<HealthResultLog> = results
                .into_iter()
                .map(|r| {
                    let (status_text, status_type) = match r.status.as_str() {
                        "completed" => ("✓ Fixed", "synced"),
                        "failed" => ("✗ Failed", "error"),
                        "skipped" => ("Skipped", "plain"),
                        _ => ("In progress", "plain"),
                    };
                    let score_str = r
                        .score
                        .map(|s| format!("{:.0}%", s * 100.0))
                        .unwrap_or_else(|| "--".to_string());
                    HealthResultLog {
                        timestamp: format_unix_timestamp(r.updated_at).into(),
                        title: r.title.into(),
                        artist: r.artist.into(),
                        provider: r.provider.unwrap_or_default().into(),
                        score_text: score_str.into(),
                        selection_reason: r
                            .selection_reason
                            .unwrap_or_else(|| "Standard match".to_string())
                            .into(),
                        status_text: status_text.into(),
                        status_type: status_type.into(),
                    }
                })
                .collect();
            window.set_health_result_logs(ModelRc::new(VecModel::from(log_rows)));
        }
    }

    refresh_filtered_health_issues(window, &issues);
}

pub(crate) fn refresh_filtered_health_issues(window: &MainWindow, issues: &[HealthIssue]) {
    let filter = window.get_health_issue_filter();
    let search = window.get_health_search_text().to_ascii_lowercase();

    let rows: Vec<HealthTrackRow> = issues
        .iter()
        .filter(|issue| {
            if !search.is_empty() {
                let haystack = format!(
                    "{} {} {}",
                    issue.title.to_ascii_lowercase(),
                    issue.artist.to_ascii_lowercase(),
                    issue.album.to_ascii_lowercase()
                );
                if !haystack.contains(&search) {
                    return false;
                }
            }
            match filter {
                1 => issue.category == HealthCategory::Missing,
                2 => issue.category == HealthCategory::WrongSong,
                3 => issue.category == HealthCategory::Offset,
                4 => issue.category == HealthCategory::TooShortOrLong,
                _ => true,
            }
        })
        .map(|issue| {
            let duration_str = issue
                .duration_seconds
                .map(|d| format!("{}:{:02}", d as u64 / 60, d as u64 % 60))
                .unwrap_or_else(|| "--:--".to_string());
            let status_str = match issue.status {
                LyricsStatus::Missing => "missing",
                LyricsStatus::Plain => "plain",
                LyricsStatus::Synced => "synced",
                LyricsStatus::Suspicious => "suspicious",
            };
            HealthTrackRow {
                path: issue.audio_path.clone().into(),
                title: issue.title.clone().into(),
                artist: issue.artist.clone().into(),
                album: issue.album.clone().into(),
                duration: duration_str.into(),
                status: status_str.into(),
                category: issue.category.as_str().into(),
                category_label: issue.category.display_label().into(),
                reason: issue.reason.clone().into(),
                is_safe: issue.is_safe,
                suggested_offset_ms: issue.suggested_offset_ms.unwrap_or(0) as i32,
            }
        })
        .collect();

    window.set_health_issues(ModelRc::new(VecModel::from(rows)));
}

// ---------------------------------------------------------------------------
// Preview generator for an issue
// ---------------------------------------------------------------------------

fn preview_diff_item_for_issue(
    db: &LibraryDb,
    settings: &AppSettings,
    issue: &HealthIssue,
) -> PreviewDiffItem {
    let old_text = fs::read_to_string(&issue.lrc_path).unwrap_or_default();

    if issue.category == HealthCategory::Offset {
        let offset_ms = issue.suggested_offset_ms.unwrap_or(0);
        let new_text = apply_offset_to_lrc(&old_text, offset_ms);
        let diff_text = generate_unified_diff(
            &old_text,
            &new_text,
            "Current Lyrics (Offset)",
            "Adjusted Lyrics",
        );
        PreviewDiffItem {
            path: issue.audio_path.clone().into(),
            title: issue.title.clone().into(),
            artist: issue.artist.clone().into(),
            issue_category: issue.category.display_label().into(),
            action_label: format!("Adjust offset ({:+.2}s)", offset_ms as f64 / 1000.0).into(),
            provider: "Internal (Offset)".into(),
            score_text: "100%".into(),
            selection_reason: "Detected timestamp offset correction".into(),
            diff_text: diff_text.into(),
        }
    } else {
        let fake_track = Track {
            audio_path: issue.audio_path.clone(),
            lrc_path: issue.lrc_path.clone(),
            artist: issue.artist.clone(),
            title: issue.title.clone(),
            album: issue.album.clone(),
            duration_seconds: issue.duration_seconds,
            status: issue.status,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };

        let mode = match issue.status {
            LyricsStatus::Missing => DownloadMode::MissingOnly,
            _ => DownloadMode::SmartUpdate,
        };

        let candidates = providers_for_settings(db, &fake_track, settings)
            .ok()
            .and_then(|providers| {
                download_track_dry_run(
                    &fake_track,
                    mode,
                    &providers,
                    &AtomicBool::new(false),
                    Some(0.70),
                )
                .ok()
            })
            .unwrap_or_default();

        let (diff_text, provider_name, score_text, reason) = if let Some(best) = candidates.first()
        {
            let new_text = best.candidate.best_text().unwrap_or_default();
            (
                generate_unified_diff(
                    &old_text,
                    new_text,
                    if old_text.is_empty() {
                        "Missing"
                    } else {
                        "Current Lyrics"
                    },
                    &format!("Proposed ({})", best.candidate.provider.label()),
                ),
                best.candidate.provider.label().to_string(),
                format!("{:.0}%", best.rank_score * 100.0),
                best.selection_reason
                    .clone()
                    .unwrap_or_else(|| "Highest score match".to_string()),
            )
        } else {
            (
                format!(
                    "--- Current Lyrics\n{}\n\n(No online provider candidates found)",
                    old_text
                ),
                "None".to_string(),
                "--".to_string(),
                "No provider match found".to_string(),
            )
        };

        PreviewDiffItem {
            path: issue.audio_path.clone().into(),
            title: issue.title.clone().into(),
            artist: issue.artist.clone().into(),
            issue_category: issue.category.display_label().into(),
            action_label: "Inspect / Fix".into(),
            provider: provider_name.into(),
            score_text: score_text.into(),
            selection_reason: reason.into(),
            diff_text: diff_text.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Health Navigation & Filters
// ---------------------------------------------------------------------------

pub(crate) fn wire_health_navigation_and_filters(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
) {
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        window.on_health_re_audit(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(s) = state.lock() {
                load_health_into_ui(&w, &s.all_tracks);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        window.on_health_filter_changed(move |filter| {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(s) = state.lock() {
                let (_, issues) = compute_health_summary(&s.all_tracks);
                w.set_health_issue_filter(filter);
                refresh_filtered_health_issues(&w, &issues);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        window.on_health_search_changed(move |search| {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(s) = state.lock() {
                let (_, issues) = compute_health_summary(&s.all_tracks);
                w.set_health_search_text(search);
                refresh_filtered_health_issues(&w, &issues);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_health_view_tab_changed(move |tab| {
            let Some(w) = weak.upgrade() else { return };
            w.set_health_active_view_tab(tab);
            if tab == 1 {
                if let Ok(db) = open_database() {
                    if let Ok(results) = db.get_latest_batch_results(60) {
                        let log_rows: Vec<HealthResultLog> = results
                            .into_iter()
                            .map(|r| {
                                let (status_text, status_type) = match r.status.as_str() {
                                    "completed" => ("✓ Fixed", "synced"),
                                    "failed" => ("✗ Failed", "error"),
                                    "skipped" => ("Skipped", "plain"),
                                    _ => ("In progress", "plain"),
                                };
                                let score_str = r
                                    .score
                                    .map(|s| format!("{:.0}%", s * 100.0))
                                    .unwrap_or_else(|| "--".to_string());
                                HealthResultLog {
                                    timestamp: format_unix_timestamp(r.updated_at).into(),
                                    title: r.title.into(),
                                    artist: r.artist.into(),
                                    provider: r.provider.unwrap_or_default().into(),
                                    score_text: score_str.into(),
                                    selection_reason: r
                                        .selection_reason
                                        .unwrap_or_else(|| "Standard match".to_string())
                                        .into(),
                                    status_text: status_text.into(),
                                    status_type: status_type.into(),
                                }
                            })
                            .collect();
                        w.set_health_result_logs(ModelRc::new(VecModel::from(log_rows)));
                    }
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_health_toggle_backup(move |enabled| {
            let Some(w) = weak.upgrade() else { return };
            w.set_settings_backup_before_overwrite(enabled);
            if let Ok(mut settings) = load_settings() {
                settings.backup_lyrics_before_overwrite = enabled;
                let _ = save_settings(&settings);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Health Preview
// ---------------------------------------------------------------------------

pub(crate) fn wire_health_preview(window: &MainWindow, app_state: &Arc<Mutex<AppState>>) {
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        window.on_health_fix_all_safe(move || {
            let Some(w) = weak.upgrade() else { return };
            let Ok(s) = state.lock() else { return };
            let (_, issues) = compute_health_summary(&s.all_tracks);
            let safe_issues: Vec<HealthIssue> = issues.into_iter().filter(|i| i.is_safe).collect();
            drop(s);

            if safe_issues.is_empty() {
                return;
            }

            let Ok(db) = open_database() else { return };
            let Ok(settings) = load_settings() else { return };

            let event_loop = w.as_weak();
            thread::spawn(move || {
                let preview_items: Vec<PreviewDiffItem> = safe_issues
                    .iter()
                    .map(|issue| preview_diff_item_for_issue(&db, &settings, issue))
                    .collect();

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = event_loop.upgrade() {
                        w.set_health_preview_items(ModelRc::new(VecModel::from(preview_items)));
                        w.set_health_preview_selected_index(0);
                        w.set_health_preview_modal_open(true);
                    }
                });
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        window.on_health_preview_single_issue(move |idx| {
            let Some(w) = weak.upgrade() else { return };
            let Ok(s) = state.lock() else { return };
            let (_, issues) = compute_health_summary(&s.all_tracks);
            let filter = w.get_health_issue_filter();
            let search = w.get_health_search_text().to_ascii_lowercase();

            let filtered_issues: Vec<HealthIssue> = issues
                .into_iter()
                .filter(|issue| {
                    if !search.is_empty() {
                        let haystack = format!(
                            "{} {} {}",
                            issue.title.to_ascii_lowercase(),
                            issue.artist.to_ascii_lowercase(),
                            issue.album.to_ascii_lowercase()
                        );
                        if !haystack.contains(&search) {
                            return false;
                        }
                    }
                    match filter {
                        1 => issue.category == HealthCategory::Missing,
                        2 => issue.category == HealthCategory::WrongSong,
                        3 => issue.category == HealthCategory::Offset,
                        4 => issue.category == HealthCategory::TooShortOrLong,
                        _ => true,
                    }
                })
                .collect();
            drop(s);

            let Some(issue) = filtered_issues.get(idx as usize).cloned() else { return };
            let Ok(db) = open_database() else { return };
            let Ok(settings) = load_settings() else { return };

            let event_loop = w.as_weak();
            thread::spawn(move || {
                let preview_items = vec![preview_diff_item_for_issue(&db, &settings, &issue)];
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = event_loop.upgrade() {
                        w.set_health_preview_items(ModelRc::new(VecModel::from(preview_items)));
                        w.set_health_preview_selected_index(0);
                        w.set_health_preview_modal_open(true);
                    }
                });
            });
        });
    }
    {
        let weak = window.as_weak();
        window.on_health_cancel_preview(move || {
            if let Some(w) = weak.upgrade() {
                w.set_health_preview_modal_open(false);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Health Batch Execution Helpers
// ---------------------------------------------------------------------------

fn spawn_apply_fixes_job(
    preview_items: Vec<PreviewDiffItem>,
    backup_enabled: bool,
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    job_id: i64,
    event_loop: slint::Weak<MainWindow>,
    state: Arc<Mutex<AppState>>,
    art: Arc<ArtLoader>,
) {
    let total = preview_items.len();
    thread::spawn(move || {
        let Ok(db) = open_database() else { return };
        let Ok(settings) = load_settings() else { return };
        let mut was_cancelled = false;

        for (idx, item) in preview_items.into_iter().enumerate() {
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

            let el = event_loop.clone();
            let cur_title = format!("{} — {}", item.artist, item.title);
            let prog = idx as f32 / total as f32;
            let prog_text = format!("{}/{} ({} remaining)", idx + 1, total, total - (idx + 1));
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = el.upgrade() {
                    w.set_health_current_track_text(cur_title.into());
                    w.set_health_progress(prog);
                    w.set_health_progress_text(prog_text.into());
                }
            });

            let audio_path = item.path.to_string();
            let lrc_path = PathBuf::from(&audio_path).with_extension("lrc");
            let track = Track {
                audio_path: audio_path.clone(),
                lrc_path: lrc_path.to_string_lossy().to_string(),
                artist: item.artist.to_string(),
                title: item.title.to_string(),
                album: String::new(),
                duration_seconds: None,
                status: LyricsStatus::Suspicious,
                mtime: None,
                last_checked: None,
                suspicious_reason: None,
                lyrics_offset_ms: 0,
            };

            let mut chosen_provider = item.provider.to_string();
            let chosen_score: Option<f64> = item
                .score_text
                .trim_end_matches('%')
                .parse::<f64>()
                .ok()
                .map(|s| s / 100.0);
            let chosen_reason = item.selection_reason.to_string();
            let mut is_success = false;

            if item.issue_category == "Offset" {
                let old_text = fs::read_to_string(&lrc_path).unwrap_or_default();
                let offset_ms = parse_offset_from_reason(&item.action_label).unwrap_or(0);
                let new_text = apply_offset_to_lrc(&old_text, offset_ms);
                if write_atomic_with_backup(&lrc_path, &new_text, backup_enabled).is_ok() {
                    let _ = db.update_lyrics_status(&audio_path, LyricsStatus::Synced, Some("Internal"));
                    is_success = true;
                }
            } else {
                let dl_opts = DownloadOptions {
                    backup_before_overwrite: backup_enabled,
                    ..Default::default()
                };
                let mode = if item.issue_category == "Missing" {
                    DownloadMode::MissingOnly
                } else {
                    DownloadMode::SmartUpdate
                };

                if let Ok(providers) = providers_for_settings(&db, &track, &settings) {
                    if let Ok(DownloadOutcome::Downloaded { provider, synced }) =
                        download_track_with_options(&track, mode, &providers, &cancelled, dl_opts, |_| {})
                    {
                        chosen_provider = provider.label().to_string();
                        let status = if synced { LyricsStatus::Synced } else { LyricsStatus::Plain };
                        let _ = db.update_lyrics_status(&audio_path, status, Some(provider.label()));
                        is_success = true;
                    }
                }
            }

            let status_str = if is_success { "completed" } else { "failed" };
            let _ = db.update_batch_job_item_progress(
                job_id,
                &audio_path,
                status_str,
                Some(&chosen_provider),
                chosen_score,
                Some(&chosen_reason),
                Some(item.diff_text.as_str()),
                None,
            );
        }

        let final_status = if was_cancelled { "cancelled" } else { "completed" };
        let _ = db.update_batch_job_status(job_id, final_status);

        let el = event_loop.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = el.upgrade() {
                w.set_health_is_running(false);
                w.set_health_is_paused(false);
                w.set_health_progress(1.0);
                w.set_health_current_track_text(
                    if was_cancelled {
                        "Batch fix cancelled."
                    } else {
                        "Batch fix completed!"
                    }
                    .into(),
                );
                load_library_into_ui(&w, &state, &art);
            }
        });
    });
}

fn spawn_resume_job(
    pending_items: Vec<BatchJobItemRecord>,
    job: BatchJobRecord,
    backup_enabled: bool,
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    event_loop: slint::Weak<MainWindow>,
    state: Arc<Mutex<AppState>>,
    art: Arc<ArtLoader>,
) {
    let total = job.total_tracks;
    let already_completed = job.completed_tracks;
    let job_id = job.id;

    thread::spawn(move || {
        let Ok(db) = open_database() else { return };
        let Ok(settings) = load_settings() else { return };
        let mut was_cancelled = false;

        for (offset, item) in pending_items.into_iter().enumerate() {
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

            let cur_idx = already_completed + offset + 1;
            let el = event_loop.clone();
            let cur_title = format!("{} — {}", item.artist, item.title);
            let prog = cur_idx as f32 / total as f32;
            let prog_text = format!("{}/{} ({} remaining)", cur_idx, total, total.saturating_sub(cur_idx));
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = el.upgrade() {
                    w.set_health_current_track_text(cur_title.into());
                    w.set_health_progress(prog);
                    w.set_health_progress_text(prog_text.into());
                }
            });

            let audio_path = item.audio_path.clone();
            let lrc_path = item.lrc_path.clone();
            let track = Track {
                audio_path: audio_path.clone(),
                lrc_path: item.lrc_path.clone(),
                artist: item.artist.clone(),
                title: item.title.clone(),
                album: item.album.clone(),
                duration_seconds: None,
                status: LyricsStatus::Suspicious,
                mtime: None,
                last_checked: None,
                suspicious_reason: item.issue_reason.clone(),
                lyrics_offset_ms: 0,
            };

            let mut chosen_provider = item.provider.clone().unwrap_or_else(|| "LRCLIB".to_string());
            let mut chosen_score = item.score.unwrap_or(0.95);
            let chosen_reason = item.selection_reason.clone().unwrap_or_else(|| "Resumed batch fix".to_string());
            let mut is_success = false;

            if item.issue_reason.as_deref().unwrap_or("").starts_with("Offset") {
                let old_text = fs::read_to_string(&lrc_path).unwrap_or_default();
                let offset_ms = parse_offset_from_reason(item.issue_reason.as_deref().unwrap_or("")).unwrap_or(0);
                let new_text = apply_offset_to_lrc(&old_text, offset_ms);
                if write_atomic_with_backup(std::path::Path::new(&lrc_path), &new_text, backup_enabled).is_ok() {
                    let _ = db.update_lyrics_status(&audio_path, LyricsStatus::Synced, Some("Internal"));
                    chosen_provider = "Internal".to_string();
                    chosen_score = 1.0;
                    is_success = true;
                }
            } else {
                let dl_opts = DownloadOptions {
                    backup_before_overwrite: backup_enabled,
                    ..Default::default()
                };

                if let Ok(providers) = providers_for_settings(&db, &track, &settings) {
                    if let Ok(DownloadOutcome::Downloaded { provider, synced }) =
                        download_track_with_options(&track, DownloadMode::SmartUpdate, &providers, &cancelled, dl_opts, |_| {})
                    {
                        chosen_provider = provider.label().to_string();
                        let status = if synced { LyricsStatus::Synced } else { LyricsStatus::Plain };
                        let _ = db.update_lyrics_status(&audio_path, status, Some(provider.label()));
                        is_success = true;
                    }
                }
            }

            let status_str = if is_success { "completed" } else { "failed" };
            let _ = db.update_batch_job_item_progress(
                job_id,
                &audio_path,
                status_str,
                Some(&chosen_provider),
                Some(chosen_score),
                Some(&chosen_reason),
                item.diff_preview.as_deref(),
                None,
            );
        }

        let final_status = if was_cancelled { "cancelled" } else { "completed" };
        let _ = db.update_batch_job_status(job_id, final_status);

        let el = event_loop.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = el.upgrade() {
                w.set_health_is_running(false);
                w.set_health_is_paused(false);
                w.set_health_progress(1.0);
                w.set_health_current_track_text(
                    if was_cancelled {
                        "Batch fix cancelled."
                    } else {
                        "Resumed batch fix completed!"
                    }
                    .into(),
                );
                load_library_into_ui(&w, &state, &art);
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Health Batch Runner Wiring
// ---------------------------------------------------------------------------

pub(crate) fn wire_health_batch_controls(
    window: &MainWindow,
    health_job_control: &Arc<Mutex<Option<ActiveBatchJob>>>,
) {
    {
        let weak = window.as_weak();
        window.on_health_dismiss_resumable(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(db) = open_database() {
                if let Ok(Some(job)) = db.get_resumable_batch_job() {
                    let _ = db.dismiss_batch_job(job.id);
                }
            }
            w.set_health_has_resumable_job(false);
        });
    }
    {
        let weak = window.as_weak();
        let job_ctrl = Arc::clone(health_job_control);
        window.on_health_pause_resume_batch(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(lock) = job_ctrl.lock() {
                if let Some(ref job) = *lock {
                    let next = !job.paused.load(std::sync::atomic::Ordering::Relaxed);
                    job.paused.store(next, std::sync::atomic::Ordering::Relaxed);
                    w.set_health_is_paused(next);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let job_ctrl = Arc::clone(health_job_control);
        window.on_health_stop_batch(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(mut lock) = job_ctrl.lock() {
                if let Some(ref job) = *lock {
                    job.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                    job.paused.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                *lock = None;
            }
            w.set_health_is_running(false);
            w.set_health_current_track_text("Batch fix stopped by user.".into());
        });
    }
}

pub(crate) fn wire_health_batch_runner(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
    health_job_control: &Arc<Mutex<Option<ActiveBatchJob>>>,
) {
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let art = Arc::clone(art_loader);
        let job_ctrl = Arc::clone(health_job_control);
        window.on_health_apply_fixes(move || {
            let Some(w) = weak.upgrade() else { return };
            w.set_health_preview_modal_open(false);

            let preview_items: Vec<PreviewDiffItem> = {
                let model = w.get_health_preview_items();
                let count = model.row_count();
                (0..count).filter_map(|i| model.row_data(i)).collect()
            };

            if preview_items.is_empty() { return; }

            let Ok(db) = open_database() else { return };
            let backup_enabled = w.get_settings_backup_before_overwrite();

            let drafts: Vec<BatchJobItemDraft> = preview_items
                .iter()
                .map(|item| BatchJobItemDraft {
                    audio_path: item.path.to_string(),
                    lrc_path: PathBuf::from(item.path.to_string())
                        .with_extension("lrc")
                        .to_string_lossy()
                        .to_string(),
                    artist: item.artist.to_string(),
                    title: item.title.to_string(),
                    album: String::new(),
                    issue_reason: Some(item.issue_category.to_string()),
                    diff_preview: Some(item.diff_text.to_string()),
                })
                .collect();

            let Ok(job_id) = db.create_batch_job("fix_safe_issues", "safe_issues", &drafts) else { return };

            let cancelled = Arc::new(AtomicBool::new(false));
            let paused = Arc::new(AtomicBool::new(false));
            if let Ok(mut lock) = job_ctrl.lock() {
                *lock = Some(ActiveBatchJob {
                    cancelled: Arc::clone(&cancelled),
                    paused: Arc::clone(&paused),
                });
            }

            let total = drafts.len();
            w.set_health_is_running(true);
            w.set_health_is_paused(false);
            w.set_health_progress(0.0);
            w.set_health_progress_text(format!("0 / {}", total).into());

            spawn_apply_fixes_job(
                preview_items,
                backup_enabled,
                cancelled,
                paused,
                job_id,
                w.as_weak(),
                Arc::clone(&state),
                Arc::clone(&art),
            );
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let art = Arc::clone(art_loader);
        let job_ctrl = Arc::clone(health_job_control);
        window.on_health_resume_batch(move || {
            let Some(w) = weak.upgrade() else { return };
            let Ok(db) = open_database() else { return };
            let Ok(Some(job)) = db.get_resumable_batch_job() else { return };
            let Ok(pending_items) = db.get_pending_batch_job_items(job.id) else { return };
            if pending_items.is_empty() {
                let _ = db.update_batch_job_status(job.id, "completed");
                w.set_health_has_resumable_job(false);
                return;
            }

            let backup_enabled = w.get_settings_backup_before_overwrite();
            let cancelled = Arc::new(AtomicBool::new(false));
            let paused = Arc::new(AtomicBool::new(false));
            if let Ok(mut lock) = job_ctrl.lock() {
                *lock = Some(ActiveBatchJob {
                    cancelled: Arc::clone(&cancelled),
                    paused: Arc::clone(&paused),
                });
            }

            let total = job.total_tracks;
            let already_completed = job.completed_tracks;
            w.set_health_has_resumable_job(false);
            w.set_health_is_running(true);
            w.set_health_is_paused(false);
            w.set_health_progress(already_completed as f32 / total as f32);
            w.set_health_progress_text(format!("{} / {}", already_completed, total).into());

            spawn_resume_job(
                pending_items,
                job,
                backup_enabled,
                cancelled,
                paused,
                w.as_weak(),
                Arc::clone(&state),
                Arc::clone(&art),
            );
        });
    }
    wire_health_batch_controls(window, health_job_control);
}

// ---------------------------------------------------------------------------
// Health Audit Master Wiring
// ---------------------------------------------------------------------------

pub(crate) fn wire_health_audit(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
    health_job_control: &Arc<Mutex<Option<ActiveBatchJob>>>,
) {
    wire_health_navigation_and_filters(window, app_state);
    wire_health_preview(window, app_state);
    wire_health_batch_runner(window, app_state, art_loader, health_job_control);
}
