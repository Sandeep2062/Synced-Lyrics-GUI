//! Headless integration tests for Slint desktop callbacks and handlers.
//!
//! Covers:
//! - Settings persistence (`save-settings`)
//! - Directory management (`manage-directories`, `remove-directory`, `close-directories`)
//! - Incremental scan & refresh (`refresh`)
//! - Playback seek, volume, transport controls (`volume-changed`, `seek`, `seek-to-line`, `play-pause`)
//! - Play queue interaction (`play-queue-track`)
//! - Per-track lyrics offset adjustment & fixes (`adjust-lyrics-offset`, `set-lyrics-offset`, `write-lyrics-offset-to-file`, `fix-offset-tag`, `fix-offset-rewrite`)
//! - Single track download & lyrics replacement (`fetch-track`, `replace-lyrics`)
//! - Batch download lifecycle (`download-all`, `batch-start-mode`, `batch-pause-resume`, `batch-stop`)
//! - Health audit, diff preview, and apply safe fixes (`health-fix-all-safe`, `health-preview-single-issue`, `health-apply-fixes`)
//! - Track identification & tag suggestions (`identify-track`, `accept-current-identification`, `ignore-current-identification`)

use crate::art_loader::ArtLoader;
use crate::library_model::{load_library_into_ui, open_database, AppState};
use crate::playback::{wire_playback, RodioAudioEngine, WaveformCacheMap};
use crate::downloads::ActiveBatchJob;
use crate::MainWindow;
use lyrics_core::{LrcLine, LrcWord, LyricsStatus, Track};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static TEST_MUTEX: Mutex<()> = Mutex::new(());

/// Minimal 16-bit PCM WAV writer so audio files can be generated without fixtures.
fn write_test_wav(path: &Path, sample_rate: u32, channels: u16, seconds: f32) {
    let frames = (sample_rate as f32 * seconds) as u32;
    let bits = 16u16;
    let block_align = channels * bits / 8;
    let data_len = frames * block_align as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for frame in 0..frames {
        let value = if (frame / 10) % 2 == 0 { 12_000i16 } else { -12_000 };
        for _ in 0..channels {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    std::fs::write(path, bytes).expect("write wav fixture");
}

#[allow(dead_code)]
struct TestHarness {
    temp_dir: PathBuf,
    window: MainWindow,
    app_state: Arc<Mutex<AppState>>,
    audio: Arc<Mutex<RodioAudioEngine>>,
    lyric_state: Arc<Mutex<Vec<LrcLine>>>,
    art_loader: Arc<ArtLoader>,
    waveform_cache: WaveformCacheMap,
    batch_job_control: Arc<Mutex<Option<ActiveBatchJob>>>,
    health_job_control: Arc<Mutex<Option<ActiveBatchJob>>>,
}

impl TestHarness {
    fn new(test_name: &str) -> Self {
        let temp_dir = std::env::temp_dir().join(format!("synced-lyrics-headless-{}", test_name));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        std::env::set_var("SYNCED_LYRICS_DATA_DIR", &temp_dir);

        crate::ui_snapshot::ensure_test_platform();

        let window = MainWindow::new().expect("create MainWindow");
        let audio = Arc::new(Mutex::new(RodioAudioEngine::new()));
        let lyric_state = Arc::new(Mutex::new(Vec::<LrcLine>::new()));
        let app_state = Arc::new(Mutex::new(AppState::new()));
        let art_loader = Arc::new(ArtLoader::new());
        let waveform_cache: WaveformCacheMap = Arc::new(Mutex::new(HashMap::new()));
        let batch_job_control: Arc<Mutex<Option<ActiveBatchJob>>> = Arc::new(Mutex::new(None));
        let health_job_control: Arc<Mutex<Option<ActiveBatchJob>>> = Arc::new(Mutex::new(None));

        // Wire all application components exactly like main.rs
        let _ = wire_playback(
            &window,
            &app_state,
            &audio,
            &lyric_state,
            &art_loader,
            &waveform_cache,
        );
        crate::scan_wiring::wire_scan_and_directories(&window, &app_state, &art_loader);
        crate::downloads::wire_downloads(
            &window,
            &app_state,
            &audio,
            &art_loader,
            &batch_job_control,
            &lyric_state,
        );
        crate::health_wiring::wire_health_audit(&window, &app_state, &art_loader, &health_job_control);
        crate::library_model::wire_search_and_filters(&window, &app_state, &art_loader);
        crate::lyrics_sync::wire_lyrics_sync(
            &window,
            &app_state,
            &audio,
            &lyric_state,
            &art_loader,
            &waveform_cache,
        );
        crate::library_model::wire_album_and_artist_views(&window, &app_state, &art_loader);
        crate::settings_wiring::wire_settings_and_updates(&window);

        Self {
            temp_dir,
            window,
            app_state,
            audio,
            lyric_state,
            art_loader,
            waveform_cache,
            batch_job_control,
            health_job_control,
        }
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

fn drain_until<F>(window: &MainWindow, timeout: Duration, mut predicate: F)
where
    F: FnMut(&MainWindow) -> bool,
{
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        crate::ui_snapshot::settle(30);
        if predicate(window) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("drain_until timed out after {:?}", timeout);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_all_headless_features() {
    let _guard = TEST_MUTEX.lock().unwrap();

    eprintln!("Running test_settings_persistence...");
    run_test_settings_persistence();

    eprintln!("Running test_directory_management_and_remove...");
    run_test_directory_management_and_remove();

    eprintln!("Running test_scan_and_refresh_incremental...");
    run_test_scan_and_refresh_incremental();

    eprintln!("Running test_offset_adjustment_and_file_rewrite...");
    run_test_offset_adjustment_and_file_rewrite();

    eprintln!("Running test_batch_download_modal_lifecycle...");
    run_test_batch_download_modal_lifecycle();

    eprintln!("Running test_coldhart_live_batch...");
    run_test_coldhart_live_batch();

    eprintln!("Running test_health_preview_and_apply_safe_fixes...");
    run_test_health_preview_and_apply_safe_fixes();

    eprintln!("Running test_track_identification_accept_and_ignore...");
    run_test_track_identification_accept_and_ignore();

    eprintln!("Running test_playback_transport_seek_volume...");
    run_test_playback_transport_seek_volume();

    eprintln!("Running test_queue_play_track...");
    run_test_queue_play_track();

    eprintln!("Running test_search_and_filter...");
    run_test_search_and_filter();

    eprintln!("Running test_album_and_artist_views...");
    run_test_album_and_artist_views();

    eprintln!("Running test_keyboard_shortcuts...");
    run_test_keyboard_shortcuts();

    eprintln!("Running test_view_snapshots...");
    run_test_view_snapshots();
}

fn run_test_settings_persistence() {
    let h = TestHarness::new("settings");

    h.window.set_settings_enable_lrclib(true);
    h.window.set_settings_enable_musixmatch(false);
    h.window.set_settings_musixmatch_key("test_key_123".into());
    h.window.set_settings_enable_netease(true);
    h.window.set_settings_request_interval(2.5);
    h.window.set_settings_cache_period_idx(2); // 30 days
    h.window.set_volume(0.82);
    h.window.set_settings_scan_on_startup(true);
    h.window.set_settings_backup_before_overwrite(false);

    h.window.invoke_save_settings();

    assert_eq!(h.window.get_settings_status().as_str(), "Saved");
    let saved = lyrics_core::load_settings().expect("load settings from temp dir");
    assert!(!saved.musixmatch_enabled);
    assert!(saved.netease_enabled);
    assert_eq!(saved.request_interval_secs, 2.5);
    assert_eq!(saved.retry_days, 30);
    assert!(saved.scan_on_startup);
    assert!(!saved.backup_lyrics_before_overwrite);
}

fn run_test_directory_management_and_remove() {
    let h = TestHarness::new("directories");

    // Manage directories overlay toggle
    h.window.invoke_manage_directories();
    assert!(h.window.get_directories_open());
    h.window.invoke_close_directories();
    assert!(!h.window.get_directories_open());

    // Setup directories in DB
    let db = open_database().expect("open db");
    let dir1 = h.temp_dir.join("music_dir1");
    let dir2 = h.temp_dir.join("music_dir2");
    std::fs::create_dir_all(&dir1).unwrap();
    std::fs::create_dir_all(&dir2).unwrap();
    db.add_directory(dir1.to_str().unwrap()).unwrap();
    db.add_directory(dir2.to_str().unwrap()).unwrap();

    // Verify remove_directory handler
    h.window.invoke_remove_directory(0);
    let remaining = db.directories().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0], dir2.to_str().unwrap());
    assert_eq!(h.window.get_settings_music_dirs().row_count(), 1);
}

fn run_test_scan_and_refresh_incremental() {
    let h = TestHarness::new("scan_refresh");

    let music_dir = h.temp_dir.join("library");
    std::fs::create_dir_all(&music_dir).unwrap();
    let track1 = music_dir.join("Pink Floyd - Time.wav");
    let track2 = music_dir.join("Pink Floyd - Money.wav");
    write_test_wav(&track1, 8000, 1, 1.0);
    write_test_wav(&track2, 8000, 1, 1.0);

    let db = open_database().unwrap();
    db.add_directory(music_dir.to_str().unwrap()).unwrap();

    // Trigger refresh scan
    h.window.invoke_refresh();
    drain_until(&h.window, Duration::from_secs(6), |w| {
        !w.get_is_refreshing() && w.get_tracks().row_count() == 2
    });

    assert_eq!(h.window.get_tracks().row_count(), 2);
    let tracks_in_db = db.tracks().unwrap();
    assert_eq!(tracks_in_db.len(), 2);

    // Add 3rd track and refresh again
    let track3 = music_dir.join("Pink Floyd - Us and Them.wav");
    write_test_wav(&track3, 8000, 1, 1.0);

    h.window.invoke_refresh();
    drain_until(&h.window, Duration::from_secs(6), |w| {
        !w.get_is_refreshing() && w.get_tracks().row_count() == 3
    });
    assert_eq!(h.window.get_tracks().row_count(), 3);
    assert_eq!(db.tracks().unwrap().len(), 3);
}

fn run_test_offset_adjustment_and_file_rewrite() {
    let h = TestHarness::new("offset");

    let music_dir = h.temp_dir.join("music");
    std::fs::create_dir_all(&music_dir).unwrap();
    let audio_path = music_dir.join("Radiohead - Creep.wav");
    let lrc_path = music_dir.join("Radiohead - Creep.lrc");
    write_test_wav(&audio_path, 8000, 1, 3.0);
    std::fs::write(
        &lrc_path,
        "[00:01.00]When you were here before\n[00:03.00]Couldn't look you in the eye\n",
    )
    .unwrap();

    let db = open_database().unwrap();
    let track = Track {
        audio_path: audio_path.to_str().unwrap().to_string(),
        lrc_path: lrc_path.to_str().unwrap().to_string(),
        artist: "Radiohead".into(),
        title: "Creep".into(),
        album: "Pablo Honey".into(),
        duration_seconds: Some(3.0),
        status: LyricsStatus::Synced,
        mtime: None,
        last_checked: None,
        suspicious_reason: None,
        lyrics_offset_ms: 0,
    };
    db.upsert_tracks(&[track.clone()]).unwrap();
    h.app_state.lock().unwrap().all_tracks = vec![track];
    h.app_state.lock().unwrap().filtered_tracks = vec![0];

    // Select track into active playback
    h.window.invoke_select_track(0);
    crate::ui_snapshot::settle(50);

    // Adjust offset +400ms
    h.window.invoke_adjust_lyrics_offset(400);
    assert_eq!(h.window.get_lyrics_offset_ms(), 400);
    assert_eq!(db.get_track_offset(audio_path.to_str().unwrap()).unwrap(), 400);

    // Set offset to -150ms
    h.window.invoke_set_lyrics_offset(-150);
    assert_eq!(h.window.get_lyrics_offset_ms(), -150);
    assert_eq!(db.get_track_offset(audio_path.to_str().unwrap()).unwrap(), -150);

    // Write offset tag to file
    h.window.invoke_write_lyrics_offset_to_file();
    crate::ui_snapshot::settle(50);
    let content = std::fs::read_to_string(&lrc_path).unwrap();
    assert!(
        content.contains("[offset: -150]") || content.contains("[offset:-150]"),
        "file should contain offset tag: {content}"
    );
    assert_eq!(h.window.get_lyrics_offset_ms(), 0);

    // Fix offset rewrite (shifts timestamps directly using estimated offset)
    h.window.set_lyrics_estimated_offset(-0.15);
    h.window.invoke_fix_offset_rewrite();
    crate::ui_snapshot::settle(50);
    let rewritten = std::fs::read_to_string(&lrc_path).unwrap();
    assert!(
        rewritten.contains("[00:01.15]"),
        "timestamp should be shifted: {rewritten}"
    );
}

fn run_test_batch_download_modal_lifecycle() {
    let h = TestHarness::new("batch");

    let t1 = Track {
        audio_path: "t1.wav".into(),
        lrc_path: "t1.lrc".into(),
        artist: "A".into(),
        title: "T1".into(),
        album: "".into(),
        duration_seconds: Some(10.0),
        status: LyricsStatus::Missing,
        mtime: None,
        last_checked: None,
        suspicious_reason: None,
        lyrics_offset_ms: 0,
    };
    let t2 = Track {
        audio_path: "t2.wav".into(),
        lrc_path: "t2.lrc".into(),
        artist: "A".into(),
        title: "T2".into(),
        album: "".into(),
        duration_seconds: Some(10.0),
        status: LyricsStatus::Plain,
        mtime: None,
        last_checked: None,
        suspicious_reason: None,
        lyrics_offset_ms: 0,
    };
    h.app_state.lock().unwrap().all_tracks = vec![t1, t2];

    // Open batch dialog
    h.window.invoke_download_all();
    assert!(h.window.get_batch_download_open());
    assert_eq!(h.window.get_batch_total_count(), 2);
    assert_eq!(h.window.get_batch_missing_count(), 1);
    assert_eq!(h.window.get_batch_plain_count(), 1);

    // Start mode 1 (MissingOnly)
    h.window.invoke_batch_start_mode(1);
    assert!(h.window.get_batch_is_running());
    assert_eq!(h.window.get_batch_download_stage(), 1);

    // Pause
    h.window.invoke_batch_pause_resume();
    assert!(h.window.get_batch_is_paused());

    // Resume
    h.window.invoke_batch_pause_resume();
    assert!(!h.window.get_batch_is_paused());

    // Stop
    h.window.invoke_batch_stop();
    assert!(!h.window.get_batch_is_running());
    assert!(h.window.get_batch_is_finished());

    // Close
    h.window.invoke_batch_close();
    assert!(!h.window.get_batch_download_open());
}

fn run_test_coldhart_live_batch() {
    let h = TestHarness::new("coldhart_live_batch");
    let music_dir = h.temp_dir.join("music");
    std::fs::create_dir_all(&music_dir).unwrap();
    let audio_path = music_dir.join("Cold Hart - Anxiety.wav");
    write_test_wav(&audio_path, 8000, 1, 3.0);

    let db = open_database().unwrap();
    db.add_directory(music_dir.to_str().unwrap()).unwrap();

    let track = Track {
        audio_path: audio_path.to_str().unwrap().into(),
        lrc_path: music_dir.join("Cold Hart - Anxiety.lrc").to_str().unwrap().into(),
        artist: "Cold Hart".into(),
        title: "Anxiety".into(),
        album: "anxious".into(),
        duration_seconds: Some(3.0),
        status: LyricsStatus::Missing,
        mtime: None,
        last_checked: None,
        suspicious_reason: None,
        lyrics_offset_ms: 0,
    };
    db.upsert_tracks(&[track.clone()]).unwrap();
    h.app_state.lock().unwrap().all_tracks = vec![track];

    // Start live batch download in MissingOnly mode
    h.window.invoke_download_all();
    assert!(h.window.get_batch_download_open());
    h.window.invoke_batch_start_mode(1); // Missing Only mode

    // Allow the real live download worker to query live providers (LRCLIB, NetEase, Musixmatch, Genius)
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        crate::ui_snapshot::settle(50);
        if h.window.get_batch_is_finished() {
            break;
        }
    }

    assert!(h.window.get_batch_is_finished(), "live batch download should finish");
    let stats = h.window.get_batch_stats();
    assert_eq!(stats.total, 1, "Only 1 track is missing");
    assert_eq!(stats.processed, 1, "Track was processed by live providers");
    h.window.invoke_batch_close();
}

fn run_test_health_preview_and_apply_safe_fixes() {
    let h = TestHarness::new("health");

    let music_dir = h.temp_dir.join("music");
    std::fs::create_dir_all(&music_dir).unwrap();
    let audio_path = music_dir.join("Queen - Under Pressure.wav");
    let lrc_path = music_dir.join("Queen - Under Pressure.lrc");
    write_test_wav(&audio_path, 8000, 1, 5.0);
    std::fs::write(
        &lrc_path,
        "[00:01.00]Pressure pushing down on me\n[00:03.00]Pressing down on you\n",
    )
    .unwrap();

    let track = Track {
        audio_path: audio_path.to_str().unwrap().into(),
        lrc_path: lrc_path.to_str().unwrap().into(),
        artist: "Queen".into(),
        title: "Under Pressure".into(),
        album: "Hot Space".into(),
        duration_seconds: Some(5.0),
        status: LyricsStatus::Suspicious,
        mtime: None,
        last_checked: None,
        suspicious_reason: Some("Offset: +0.50s".into()),
        lyrics_offset_ms: 0,
    };
    let db = open_database().unwrap();
    db.upsert_tracks(&[track.clone()]).unwrap();
    h.app_state.lock().unwrap().all_tracks = vec![track];

    // Re-audit
    h.window.invoke_health_re_audit();
    assert_eq!(h.window.get_health_suspicious_count(), 1);
    assert_eq!(h.window.get_health_safe_issues_count(), 1);

    // Preview safe fix
    h.window.invoke_health_fix_all_safe();
    drain_until(&h.window, Duration::from_secs(4), |w| {
        w.get_health_preview_modal_open()
    });
    assert!(h.window.get_health_preview_modal_open());
    assert_eq!(h.window.get_health_preview_items().row_count(), 1);

    // Apply fixes
    h.window.invoke_health_apply_fixes();
    drain_until(&h.window, Duration::from_secs(6), |w| {
        !w.get_health_is_running()
    });

    // Verify backup .bak file was created and lrc file was modified
    let bak_path = music_dir.join("Queen - Under Pressure.lrc.bak");
    assert!(bak_path.exists(), "backup .bak file should exist");
    let fixed_lrc = std::fs::read_to_string(&lrc_path).unwrap();
    assert!(
        fixed_lrc.contains("[offset: 500]") || fixed_lrc.contains("[offset:500]") || fixed_lrc.contains("[00:01.50]"),
        "lrc should be fixed: {fixed_lrc}"
    );
}

fn run_test_track_identification_accept_and_ignore() {
    let h = TestHarness::new("identify");

    let music_dir = h.temp_dir.join("music");
    std::fs::create_dir_all(&music_dir).unwrap();
    let audio_path = music_dir.join("Track 01.wav");
    write_test_wav(&audio_path, 8000, 1, 2.0);

    let track = Track {
        audio_path: audio_path.to_str().unwrap().into(),
        lrc_path: music_dir.join("Track 01.lrc").to_str().unwrap().into(),
        artist: "Unknown Artist".into(),
        title: "Track 01".into(),
        album: "Unknown Album".into(),
        duration_seconds: Some(2.0),
        status: LyricsStatus::Missing,
        mtime: None,
        last_checked: None,
        suspicious_reason: None,
        lyrics_offset_ms: 0,
    };
    let db = open_database().unwrap();
    db.upsert_tracks(&[track.clone()]).unwrap();
    h.app_state.lock().unwrap().all_tracks = vec![track];
    h.app_state.lock().unwrap().filtered_tracks = vec![0];
    h.app_state.lock().unwrap().current_filtered_row = Some(0);

    // Populate suggestion
    h.window.set_lyrics_suggested_artist("The Beatles".into());
    h.window.set_lyrics_suggested_title("Let It Be".into());
    h.window.set_lyrics_suggested_album("Let It Be".into());
    h.window.set_lyrics_has_identification_suggestion(true);

    // Test ignore
    h.window.invoke_ignore_current_identification();
    assert!(!h.window.get_lyrics_has_identification_suggestion());

    // Populate suggestion again and accept
    h.window.set_lyrics_suggested_artist("The Beatles".into());
    h.window.set_lyrics_suggested_title("Let It Be".into());
    h.window.set_lyrics_suggested_album("Let It Be".into());
    h.window.set_lyrics_has_identification_suggestion(true);

    h.window.invoke_accept_current_identification();
    drain_until(&h.window, Duration::from_secs(6), |w| {
        w.get_track_title() == "Let It Be"
    });

    assert_eq!(h.window.get_track_title(), "Let It Be");
    let tracks = db.tracks().unwrap();
    assert_eq!(tracks[0].title, "Let It Be");
    assert_eq!(tracks[0].artist, "The Beatles");
}

fn run_test_playback_transport_seek_volume() {
    let h = TestHarness::new("playback");

    // 1. Volume and Mute persistence
    h.window.invoke_volume_changed(0.85);
    assert!((h.window.get_volume() - 0.85).abs() < 1e-4);
    assert!((h.audio.lock().unwrap().volume - 0.85).abs() < 1e-4);
    assert!(!h.window.get_is_muted());
    let saved_settings = lyrics_core::load_settings().expect("load settings");
    assert!((saved_settings.volume - 0.85).abs() < 1e-4);
    assert!(!saved_settings.muted);

    // Mute toggle
    h.window.invoke_toggle_mute();
    assert!(h.window.get_is_muted());
    assert_eq!(h.audio.lock().unwrap().volume, 0.0);
    let muted_settings = lyrics_core::load_settings().expect("load settings");
    assert!(muted_settings.muted);

    // Unmute
    h.window.invoke_toggle_mute();
    assert!(!h.window.get_is_muted());
    assert!((h.audio.lock().unwrap().volume - 0.85).abs() < 1e-4);
    let unmuted_settings = lyrics_core::load_settings().expect("load settings");
    assert!(!unmuted_settings.muted);

    // 2. Lyrics seek & Click-to-seek
    let lines = vec![
        LrcLine { timestamp_seconds: 1.0, text: "Line 1".into(), words: vec![] },
        LrcLine {
            timestamp_seconds: 3.0,
            text: "Hello World".into(),
            words: vec![
                LrcWord { text: "Hello".into(), timestamp_seconds: 3.0 },
                LrcWord { text: "World".into(), timestamp_seconds: 4.0 },
            ],
        },
        LrcLine { timestamp_seconds: 5.0, text: "Line 3".into(), words: vec![] },
    ];
    *h.lyric_state.lock().unwrap() = lines;
    h.audio.lock().unwrap().duration = Some(10.0);

    // Seek to line 1 (index 1 -> 3.0s) via click-to-seek
    h.window.invoke_seek_to_line(1);
    assert_eq!(h.window.get_active_line_index(), 1);
    assert_eq!(h.window.get_position_text(), "0:03");
    assert!((h.window.get_seek_position() - 0.30).abs() < 1e-3);
    assert!(h.window.get_active_has_words());
    let row1 = h.window.get_active_words_row1();
    assert_eq!(row1.row_count(), 2);
    let w0 = row1.row_data(0).unwrap();
    let w1 = row1.row_data(1).unwrap();
    assert_eq!(w0.text, "Hello");
    assert!(w0.is_sung);
    assert!(w0.is_current);
    assert_eq!(w1.text, "World");
    assert!(!w1.is_sung);
    assert!(!w1.is_current);

    // 3. Drag-scrub waveform seeking
    h.window.invoke_seek(0.45); // 4.5s -> line 1 (3.0s), word 1 ("World" at 4.0s is now active!)
    assert_eq!(h.window.get_active_line_index(), 1);
    assert_eq!(h.window.get_position_text(), "0:04");
    assert!((h.window.get_seek_position() - 0.45).abs() < 1e-3);
    let row1_updated = h.window.get_active_words_row1();
    let w1_updated = row1_updated.row_data(1).unwrap();
    assert!(w1_updated.is_sung);
    assert!(w1_updated.is_current);

    // 4. Per-track offset buttons
    h.window.invoke_adjust_lyrics_offset(100);
    assert_eq!(h.window.get_lyrics_offset_ms(), 100);
    h.window.invoke_adjust_lyrics_offset(-500);
    assert_eq!(h.window.get_lyrics_offset_ms(), -400);
    h.window.invoke_set_lyrics_offset(0);
    assert_eq!(h.window.get_lyrics_offset_ms(), 0);

    // 5. Modes toggling: now-playing view, queue drawer, side lyrics, mini-player
    h.window.set_has_track(true);
    h.window.set_has_lyrics(true);
    assert!(!h.window.get_now_playing_visible());
    h.window.set_now_playing_visible(true);
    assert!(h.window.get_now_playing_visible());
    h.window.set_now_playing_visible(false);

    assert!(!h.window.get_mini_player_mode());
    h.window.set_mini_player_mode(true);
    assert!(h.window.get_mini_player_mode());
    h.window.set_mini_player_mode(false);

    assert!(!h.window.get_lyrics_visible());
    h.window.set_lyrics_visible(true);
    assert!(h.window.get_lyrics_visible());
    h.window.set_lyrics_visible(false);

    assert!(!h.window.get_queue_visible());
    h.window.set_queue_visible(true);
    assert!(h.window.get_queue_visible());
    h.window.set_queue_visible(false);
}

fn run_test_queue_play_track() {
    let h = TestHarness::new("queue");

    let t1 = Track {
        audio_path: "q1.wav".into(),
        lrc_path: "q1.lrc".into(),
        artist: "Queue Artist 1".into(),
        title: "Queue Title 1".into(),
        album: "Queue Album".into(),
        duration_seconds: Some(120.0),
        status: LyricsStatus::Synced,
        mtime: None,
        last_checked: None,
        suspicious_reason: None,
        lyrics_offset_ms: 0,
    };
    let t2 = Track {
        audio_path: "q2.wav".into(),
        lrc_path: "q2.lrc".into(),
        artist: "Queue Artist 2".into(),
        title: "Queue Title 2".into(),
        album: "Queue Album".into(),
        duration_seconds: Some(140.0),
        status: LyricsStatus::Synced,
        mtime: None,
        last_checked: None,
        suspicious_reason: None,
        lyrics_offset_ms: 0,
    };
    h.app_state.lock().unwrap().all_tracks = vec![t1.clone(), t2.clone()];
    h.app_state.lock().unwrap().filtered_tracks = vec![0, 1];

    let ui_t1 = crate::track_to_ui(&t1, None);
    let ui_t2 = crate::track_to_ui(&t2, None);
    h.window.set_queue_tracks(ModelRc::new(VecModel::from(vec![ui_t1, ui_t2])));

    h.window.invoke_play_queue_track(1);
    crate::ui_snapshot::settle(50);
    assert_eq!(h.window.get_track_title(), "Queue Title 2");
}

fn run_test_search_and_filter() {
    let h = TestHarness::new("search_filter");

    let t1 = Track {
        audio_path: "a.wav".into(),
        lrc_path: "a.lrc".into(),
        artist: "Coldplay".into(),
        title: "Yellow".into(),
        album: "Parachutes".into(),
        duration_seconds: Some(200.0),
        status: LyricsStatus::Synced,
        mtime: None,
        last_checked: None,
        suspicious_reason: None,
        lyrics_offset_ms: 0,
    };
    let t2 = Track {
        audio_path: "b.wav".into(),
        lrc_path: "b.lrc".into(),
        artist: "Coldplay".into(),
        title: "Clocks".into(),
        album: "A Rush of Blood".into(),
        duration_seconds: Some(200.0),
        status: LyricsStatus::Missing,
        mtime: None,
        last_checked: None,
        suspicious_reason: None,
        lyrics_offset_ms: 0,
    };
    let db = open_database().unwrap();
    db.upsert_tracks(&[t1, t2]).unwrap();
    load_library_into_ui(&h.window, &h.app_state, &h.art_loader);

    assert_eq!(h.window.get_tracks().row_count(), 2);

    // Search filter
    h.window.invoke_search_changed("Yellow".into());
    assert_eq!(h.window.get_tracks().row_count(), 1);

    // Status filter (status 1 = Missing)
    h.window.invoke_search_changed("".into());
    h.window.invoke_filter_changed(1);
    assert_eq!(h.window.get_tracks().row_count(), 1);
    assert_eq!(h.window.get_tracks().row_data(0).unwrap().title, "Clocks");

    // Status filter (status 3 = Synced)
    h.window.invoke_filter_changed(3);
    assert_eq!(h.window.get_tracks().row_count(), 1);
    assert_eq!(h.window.get_tracks().row_data(0).unwrap().title, "Yellow");
}

fn run_test_album_and_artist_views() {
    let h = TestHarness::new("album_artist");

    let t1 = Track {
        audio_path: "a.wav".into(),
        lrc_path: "a.lrc".into(),
        artist: "Nirvana".into(),
        title: "Smells Like Teen Spirit".into(),
        album: "Nevermind".into(),
        duration_seconds: Some(300.0),
        status: LyricsStatus::Synced,
        mtime: None,
        last_checked: None,
        suspicious_reason: None,
        lyrics_offset_ms: 0,
    };
    let db = open_database().unwrap();
    db.upsert_tracks(&[t1]).unwrap();
    load_library_into_ui(&h.window, &h.app_state, &h.art_loader);

    assert_eq!(h.window.get_albums().row_count(), 1);
    h.window.invoke_open_album(0);
    assert!(h.window.get_album_detail_mode());
    assert_eq!(h.window.get_album_tracks().row_count(), 1);

    h.window.invoke_back_to_album_grid();
    assert!(!h.window.get_album_detail_mode());

    assert_eq!(h.window.get_artists().row_count(), 1);
    h.window.invoke_select_artist(0);
    assert_eq!(h.window.get_artist_tracks().row_count(), 1);
}

fn run_test_keyboard_shortcuts() {
    let h = TestHarness::new("shortcuts");
    h.window.set_has_track(true);
    h.window.set_has_lyrics(true);

    // Initial state
    assert!(!h.window.get_now_playing_visible());
    assert!(!h.window.get_is_muted());

    // 'f' toggles now-playing (fullscreen)
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "f".into() });
    assert!(h.window.get_now_playing_visible());

    // Escape exits now-playing
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
    assert!(!h.window.get_now_playing_visible());

    // 'm' toggles mute
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "m".into() });
    assert!(h.window.get_is_muted());
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "m".into() });
    assert!(!h.window.get_is_muted());

    // 'p' toggles mini player
    assert!(!h.window.get_mini_player_mode());
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "p".into() });
    assert!(h.window.get_mini_player_mode());
    // Escape exits mini player
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
    assert!(!h.window.get_mini_player_mode());

    // 'l' toggles lyrics panel
    assert!(!h.window.get_lyrics_visible());
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "l".into() });
    assert!(h.window.get_lyrics_visible());

    // 'q' toggles queue and closes lyrics
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "q".into() });
    assert!(h.window.get_queue_visible());
    assert!(!h.window.get_lyrics_visible());

    // Escape closes queue
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
    assert!(!h.window.get_queue_visible());

    // Offset shortcuts when lyrics panel is open
    h.window.set_lyrics_visible(true);
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "]".into() });
    assert_eq!(h.window.get_lyrics_offset_ms(), 100);
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "}".into() });
    assert_eq!(h.window.get_lyrics_offset_ms(), 600);
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "[".into() });
    assert_eq!(h.window.get_lyrics_offset_ms(), 500);
    h.window.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "0".into() });
    assert_eq!(h.window.get_lyrics_offset_ms(), 0);
}

fn run_test_view_snapshots() {
    let h = TestHarness::new("view_snapshots");
    h.window.set_has_track(true);
    h.window.set_track_title("Cruel Angel's Thesis".into());
    h.window.set_track_subtitle("Yoko Takahashi - Evangelion".into());
    h.window.set_duration_text("4:05".into());
    h.window.set_position_text("1:24".into());
    h.window.set_seek_position(0.34);
    h.window.set_volume(0.75);
    h.window.set_is_muted(false);

    h.window.set_lyrics_lines(ModelRc::new(VecModel::from(crate::ui_snapshot::worst_case_lyrics())));
    h.window.set_has_lyrics(true);
    h.window.set_lyrics_status("synced".into());
    h.window.set_active_line_index(2);
    h.window.set_lyrics_offset_ms(100);

    // Populate waveform with 100 sample bars
    let bars: Vec<f32> = (0..100)
        .map(|i| {
            let x = i as f32 / 100.0;
            0.15 + 0.7 * (x * std::f32::consts::PI * 4.0).sin().abs()
        })
        .collect();
    h.window.set_waveform_data(ModelRc::new(VecModel::from(bars)));

    // View 1: Now Playing Fullscreen with Synced Lyrics
    h.window.set_now_playing_visible(true);
    h.window.set_queue_visible(false);
    h.window.set_lyrics_visible(false);
    h.window.set_mini_player_mode(false);
    crate::ui_snapshot::save_frame_png("view-08-now-playing-synced-1280", 1280, 820);
    h.window.set_now_playing_visible(false);

    // Setup queue
    h.window.set_queue_tracks(ModelRc::new(VecModel::from(crate::ui_snapshot::worst_case_tracks(5))));
    h.window.set_current_queue_index(0);

    // View 2: Queue Drawer
    h.window.set_queue_visible(true);
    crate::ui_snapshot::save_frame_png("view-09-queue-drawer-1280", 1280, 820);
    h.window.set_queue_visible(false);

    // View 3: Side Lyrics Panel
    h.window.set_lyrics_visible(true);
    crate::ui_snapshot::save_frame_png("view-10-lyrics-synced-1280", 1280, 820);
    h.window.set_lyrics_visible(false);

    // View 4: Mini-Player Floating Mode
    h.window.set_mini_player_mode(true);
    crate::ui_snapshot::save_frame_png("view-13-mini-player-1280", 1280, 820);
    h.window.set_mini_player_mode(false);
}
