#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod audio_source;
mod art_loader;
pub(crate) use art_loader::*;
mod library_model;
pub(crate) use library_model::*;

use lofty::prelude::AudioFile;
use lyrics_core::{
    apply_offset_to_lrc, compute_health_summary, download_track,
    download_track_dry_run, download_track_with_options, find_active_line_index,
    find_active_word_index, generate_unified_diff, load_settings, parse_lines,
    parse_offset_from_reason, save_settings, scan_directory_incremental, write_atomic_with_backup,
    AppSettings, AudioEngine, BatchJobItemDraft, DownloadEvent, DownloadMode, DownloadOptions,
    DownloadOutcome, GeniusProvider, HealthCategory, HealthIssue, LibraryDb, LrcLine, LrcWord,
    LrclibProvider, LyricsProvider, LyricsStatus, MegalobizProvider, MusixmatchProvider,
    NeteaseProvider, ProviderLyrics, ProviderName, Track, WaveformStore,
};

struct ActiveBatchJob {
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}
use rodio::Source;
use slint::{Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Audio engine
// ---------------------------------------------------------------------------

struct RodioAudioEngine {
    _stream: Option<rodio::OutputStream>,
    stream_handle: Option<rodio::OutputStreamHandle>,
    sink: Option<rodio::Sink>,
    current_path: Option<String>,
    duration: Option<f64>,
    volume: f32,
    speed: f32,
    /// Live RMS amplitude from the audio stream, updated ~every 2048 samples.
    live_level: audio_source::LiveLevel,
}

impl RodioAudioEngine {
    fn new() -> Self {
        let (_stream, stream_handle) = match rodio::OutputStream::try_default() {
            Ok((s, h)) => (Some(s), Some(h)),
            Err(e) => {
                eprintln!("Audio output device notice: {e}");
                (None, None)
            }
        };
        Self {
            _stream,
            stream_handle,
            sink: None,
            current_path: None,
            duration: None,
            volume: 0.7,
            speed: 1.0,
            live_level: audio_source::LiveLevel::new(),
        }
    }

    fn ensure_handle(&mut self) -> Result<rodio::OutputStreamHandle, String> {
        if let Some(ref h) = self.stream_handle {
            return Ok(h.clone());
        }
        let (s, h) =
            rodio::OutputStream::try_default().map_err(|e| format!("Audio device error: {e}"))?;
        self._stream = Some(s);
        self.stream_handle = Some(h.clone());
        Ok(h)
    }

    fn read_duration(path: &str) -> Option<f64> {
        let tagged = lofty::probe::Probe::open(path).ok()?.read().ok()?;
        let dur = tagged.properties().duration().as_secs_f64();
        (dur > 0.0).then_some(dur)
    }
}

impl AudioEngine for RodioAudioEngine {
    fn play(&mut self, path: &str) -> Result<(), String> {
        self.stop();
        let handle = self.ensure_handle()?;
        // Our own symphonia source, not `rodio::Decoder`: the latter panics on
        // M4A/AAC files (see `audio_source` for the gory details).
        let source = audio_source::FileSource::open(std::path::Path::new(path))?;
        let decoded_duration = source.total_duration().map(|d| d.as_secs_f64());
        // Wrap in LevelCapture so we can read live amplitude from the UI timer.
        let live = audio_source::LiveLevel::new();
        self.live_level = live.clone();
        let captured = audio_source::LevelCapture::new(source, live);
        let sink = rodio::Sink::try_new(&handle).map_err(|e| format!("Sink error: {e}"))?;
        sink.set_volume(self.volume);
        sink.set_speed(self.speed);
        sink.append(captured);
        sink.play();
        self.duration = Self::read_duration(path).or(decoded_duration);
        self.sink = Some(sink);
        self.current_path = Some(path.to_string());
        Ok(())
    }

    fn toggle_pause(&mut self) -> Result<bool, String> {
        if let Some(ref sink) = self.sink {
            if sink.is_paused() {
                sink.play();
                Ok(false)
            } else {
                sink.pause();
                Ok(true)
            }
        } else if let Some(path) = self.current_path.clone() {
            self.play(&path)?;
            Ok(false)
        } else {
            Err("No track selected".to_string())
        }
    }

    fn position_seconds(&self) -> Option<f64> {
        self.sink.as_ref().map(|s| s.get_pos().as_secs_f64())
    }

    fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
    }

    fn seek(&mut self, position_secs: f64) -> Result<(), String> {
        let sink = self.sink.as_ref().ok_or("no track loaded")?;
        sink.try_seek(Duration::from_secs_f64(position_secs))
            .map_err(|e| e.to_string())
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        if let Some(sink) = &self.sink {
            sink.set_volume(volume);
        }
    }

    fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
        if let Some(sink) = &self.sink {
            sink.set_speed(speed);
        }
    }

    fn duration_seconds(&self) -> Option<f64> {
        self.duration
    }

    fn is_playing(&self) -> bool {
        self.sink
            .as_ref()
            .map(|s| !s.is_paused() && !s.empty())
            .unwrap_or(false)
    }
}


// ---------------------------------------------------------------------------
// Waveform extraction, caching, and sound-reactive playback (Send-safe)
// ---------------------------------------------------------------------------
//
// Two things are drawn from one array. The array is the track's real loudness
// envelope — `ENVELOPE_BUCKETS` peaks spread evenly over the whole file, which
// is what [`audio_source::waveform_bars`] returns. From it we derive:
//
// * the static shape of the waveform (`WAVEFORM_BARS` bars, one per drawn bar), and
// * the live level at the playhead, which is what makes the bars move *with the
//   music* rather than to a sine wave that only pretends to.
//
// Both are recomputed on every animation tick, so the drawing follows playback
// instead of restarting a canned animation each time.

/// Decoded per-track envelopes, keyed by audio path. Shared through an `Arc` so
/// the playback timer can read one 30 times a second without copying it.
type WaveformCacheMap = Arc<Mutex<HashMap<String, Arc<Vec<f32>>>>>;

/// Bars drawn in the seek bar. Fixed: the model is written in place every tick,
/// so its length must never change while the UI is bound to it.
pub(crate) const WAVEFORM_BARS: usize = 72;
/// Loudness buckets kept per track — about 120 ms each on a 3-minute song, which
/// is fine enough for the bars to react to a drum hit.
const ENVELOPE_BUCKETS: usize = 1500;
/// How far behind the playhead (in track fraction) the live level is applied.
const PULSE_WIDTH: f32 = 0.12;

/// A clean, flat baseline shape used before a track's envelope has been decoded
/// (and for files that cannot be decoded at all).
fn synthetic_shape(count: usize) -> Vec<f32> {
    vec![0.15f32; count]
}

fn default_waveform() -> Vec<f32> {
    synthetic_shape(WAVEFORM_BARS)
}

/// Peak of each display bar's window of `envelope`. Taking the peak rather than
/// the mean keeps short, loud hits visible after down-sampling.
fn envelope_bars(envelope: &[f32], bars: usize) -> Vec<f32> {
    if envelope.is_empty() {
        return synthetic_shape(bars);
    }
    (0..bars)
        .map(|i| {
            let low = i * envelope.len() / bars;
            let high = (((i + 1) * envelope.len() / bars).max(low + 1)).min(envelope.len());
            envelope[low..high].iter().copied().fold(0.0f32, f32::max)
        })
        .collect()
}

/// Writes `values` into the waveform model, touching only the rows that actually
/// changed. Replacing the whole model (or re-setting every row) on every frame is
/// what made the old animation stutter: each write invalidated all 72 bars.
fn write_waveform_bars(model: &VecModel<f32>, values: &[f32]) {
    for (index, &value) in values.iter().enumerate() {
        if index >= model.row_count() {
            break;
        }
        let changed = model
            .row_data(index)
            .map(|current| (current - value).abs() > 0.004)
            .unwrap_or(true);
        if changed {
            model.set_row_data(index, value);
        }
    }
}

/// The bars to draw for `progress` (0..1) through the track. `level` is the
/// live RMS amplitude from the audio thread (0.0–1.0), already on a perceptual
/// (dB) curve.
///
/// Every bar is modulated by the live level — the whole waveform breathes with
/// the music. Bars near the playhead get a stronger push so the waveform also
/// reads as a progress indicator. When paused, the static envelope shape is
/// returned as-is.
fn reactive_bars(envelope: &[f32], progress: f32, level: f32, playing: bool) -> Vec<f32> {
    let mut bars = envelope_bars(envelope, WAVEFORM_BARS);
    if !playing {
        return bars;
    }
    // Square the level for a punchier feel: quiet is *very* small, loud is big.
    let intensity = level * level;

    for (index, bar) in bars.iter_mut().enumerate() {
        let fraction = (index as f32 + 0.5) / WAVEFORM_BARS as f32;
        let base = *bar;

        // Global breath: the bar swings between a tiny floor and its full
        // envelope height. The floor is deliberately very low so that during
        // quiet passages the bars visibly shrink toward the centre line.
        let floor = 0.08;
        let height = floor + (base - floor).max(0.0) * intensity;

        // Local pulse near the playhead: a bump that makes the current
        // position obvious even when the music is quiet.
        let dist = (progress - fraction).abs();
        let near = (1.0 - dist / PULSE_WIDTH).clamp(0.0, 1.0);
        let pulse = near * near * level * 0.5;

        *bar = (height + pulse).clamp(0.06, 1.0);
    }
    bars
}

/// Pushes `values` into the seek bar's waveform model in place. The model is only
/// ever mutated, never replaced, so the bars keep their identity between frames
/// and Slint can animate the change instead of rebuilding the row.
fn set_waveform_bars(window: &MainWindow, values: &[f32]) {
    let model = window.get_waveform_data();
    match model.as_any().downcast_ref::<VecModel<f32>>() {
        Some(vec) => write_waveform_bars(vec, values),
        // The model was replaced wholesale (only tests do this): fall back.
        None => window.set_waveform_data(ModelRc::new(VecModel::from(values.to_vec()))),
    }
}

fn compute_audio_waveform(audio_path: &str) -> Vec<f32> {
    // Decoded through our own source so formats rodio cannot open (M4A) still
    // get a real envelope instead of the synthetic fallback.
    audio_source::waveform_bars(std::path::Path::new(audio_path), ENVELOPE_BUCKETS, || {
        synthetic_shape(ENVELOPE_BUCKETS)
    })
}

// ---------------------------------------------------------------------------
// Cached provider
// ---------------------------------------------------------------------------

struct CachedLyricsProvider {
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

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------


fn providers_for_settings(
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

fn update_ui_words(window: &MainWindow, words: &[LrcWord], cur_word_idx: i32, current_time: f64) {
    if !window.get_active_has_words() {
        window.set_active_has_words(true);
    }
    let mut row1_words = Vec::new();
    let mut row2_words = Vec::new();
    let mut acc_len = 0;

    for (i, w) in words.iter().enumerate() {
        let is_sung = w.timestamp_seconds <= current_time;
        let is_current = i as i32 == cur_word_idx;
        let ui_word = LyricWord {
            text: w.text.as_str().into(),
            is_sung,
            is_current,
        };
        acc_len += w.text.chars().count() + 1;
        if acc_len <= 30 || row1_words.is_empty() {
            row1_words.push(ui_word);
        } else {
            row2_words.push(ui_word);
        }
    }

    window.set_active_words_row1(ModelRc::new(VecModel::from(row1_words)));
    window.set_active_words_row2(ModelRc::new(VecModel::from(row2_words)));
}

// ---------------------------------------------------------------------------
// Track setup helper (updates info, artwork, waveform, lyrics, and plays if requested)
// ---------------------------------------------------------------------------

fn setup_track_for_playback(
    track: &Track,
    window: &MainWindow,
    engine: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &ArtLoader,
    waveform_cache: &WaveformCacheMap,
    auto_play: bool,
) {
    window.set_track_title(track.title.clone().into());
    window.set_track_subtitle(format!("{} • {}", track.artist, track.album).into());
    window.set_track_album(track.album.clone().into());
    window.set_has_track(true);

    // Cover art: the track's own, or failing that its album's.
    let cover_opt =
        track_cover_now(track, art_loader).or_else(|| album_cover_image(&track.album, art_loader));
    if let Some(ref cover) = cover_opt {
        window.set_track_cover(cover.clone());
        window.set_has_cover(true);
        art_loader.set_player_album(None);
    } else {
        window.set_track_cover(slint::Image::default());
        window.set_has_cover(false);
        // Nothing to show yet. The album's artwork may still be being looked
        // for, so remember which album to fill the player in with.
        art_loader.set_player_album(Some(track.album.clone()));
    }

    // Waveform: the track's loudness envelope, decoded once and cached.
    let stamp = track.mtime.unwrap_or(0.0) as u64;
    let path = track.audio_path.clone();

    // 1. Check in-memory cache
    let cached_envelope = {
        let cache = waveform_cache.lock().unwrap();
        cache.get(&path).cloned()
    };

    // 2. Check on-disk WaveformStore if not in memory (instant <0.1ms load)
    let cached_envelope = cached_envelope.or_else(|| {
        let store = WaveformStore::default_location()?;
        let envelope = store.get(&path, stamp)?;
        let arc = Arc::new(envelope);
        if let Ok(mut cache) = waveform_cache.lock() {
            cache.insert(path.clone(), Arc::clone(&arc));
        }
        Some(arc)
    });

    if let Some(envelope) = cached_envelope {
        set_waveform_bars(window, &envelope_bars(&envelope, WAVEFORM_BARS));
    } else {
        // While decoding, set baseline shape — the live animation timer will
        // immediately modulate it with live audio level so it is responsive from frame 1!
        set_waveform_bars(window, &default_waveform());
        let wf_cache = Arc::clone(waveform_cache);
        let weak = window.as_weak();
        let bg_path = path.clone();
        thread::spawn(move || {
            let envelope = Arc::new(compute_audio_waveform(&bg_path));
            if let Some(store) = WaveformStore::default_location() {
                store.put(&bg_path, stamp, &envelope);
            }
            if let Ok(mut cache) = wf_cache.lock() {
                cache.insert(bg_path.clone(), Arc::clone(&envelope));
            }
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    set_waveform_bars(&w, &envelope_bars(&envelope, WAVEFORM_BARS));
                }
            });
        });
    }

    // Load lyrics
    let mut is_suspicious = track.status == LyricsStatus::Suspicious;
    let mut suspicious_reason = track.suspicious_reason.clone().unwrap_or_default();
    let mut has_offset = false;
    let mut estimated_offset = 0.0f32;

    let has_lyrics = if let Ok(contents) = fs::read_to_string(&track.lrc_path) {
        let lines = parse_lines(&contents);
        let is_synced = lines.iter().any(|l| l.timestamp_seconds > 0.0);
        if is_synced {
            let ui_lines: Vec<LyricLine> = lines
                .iter()
                .map(|l| LyricLine {
                    timestamp: l.timestamp_seconds as f32,
                    text: l.text.clone().into(),
                })
                .collect();
            window.set_lyrics_lines(ModelRc::new(VecModel::from(ui_lines)));
            window.set_lyrics_plain_text("".into());

            // Run audit to check for suspicious issues or offsets
            let report =
                lyrics_core::audit_lrc(&contents, track.duration_seconds, &track.title, &[]);
            if report.is_suspicious() {
                is_suspicious = true;
                if suspicious_reason.is_empty() {
                    suspicious_reason = report.reason_summary().unwrap_or_default();
                }
                if let Some(offset) = report.estimated_offset_seconds {
                    has_offset = true;
                    estimated_offset = offset as f32;
                }
            }

            window.set_lyrics_status(
                if is_suspicious {
                    "suspicious"
                } else {
                    "synced"
                }
                .into(),
            );
            if let Ok(mut ls) = lyric_state.lock() {
                *ls = lines;
            }
            true
        } else if !contents.trim().is_empty() {
            window.set_lyrics_lines(ModelRc::new(VecModel::from(Vec::<LyricLine>::new())));
            window.set_lyrics_plain_text(contents.into());
            window.set_lyrics_status("plain".into());
            if let Ok(mut ls) = lyric_state.lock() {
                ls.clear();
            }
            true
        } else {
            window.set_lyrics_lines(ModelRc::new(VecModel::from(Vec::<LyricLine>::new())));
            window.set_lyrics_plain_text("".into());
            window.set_lyrics_status("missing".into());
            if let Ok(mut ls) = lyric_state.lock() {
                ls.clear();
            }
            false
        }
    } else {
        window.set_lyrics_lines(ModelRc::new(VecModel::from(Vec::<LyricLine>::new())));
        window.set_lyrics_plain_text("".into());
        window.set_lyrics_status("missing".into());
        if let Ok(mut ls) = lyric_state.lock() {
            ls.clear();
        }
        false
    };

    window.set_lyrics_is_suspicious(is_suspicious);
    window.set_lyrics_suspicious_reason(suspicious_reason.into());
    window.set_lyrics_has_offset(has_offset);
    window.set_lyrics_estimated_offset(estimated_offset);
    window.set_lyrics_has_identification_suggestion(false);
    window.set_lyrics_is_identifying(false);
    window.set_lyrics_offset_ms(track.lyrics_offset_ms as i32);
    window.set_lyrics_offset_save_status("".into());
    window.set_active_has_words(false);
    window.set_active_words_row1(ModelRc::new(VecModel::from(Vec::<LyricWord>::new())));
    window.set_active_words_row2(ModelRc::new(VecModel::from(Vec::<LyricWord>::new())));

    // Update has_lyrics property on window for PlayerBar lyrics button visibility
    window.set_has_lyrics(has_lyrics);

    // Smoothly open side lyrics panel if track has lyrics
    if has_lyrics {
        window.set_lyrics_visible(true);
    }

    // Audio Playback
    if let Ok(mut eng) = engine.lock() {
        eng.current_path = Some(track.audio_path.clone());
        if auto_play {
            if let Err(e) = eng.play(&track.audio_path) {
                eprintln!("Audio playback error: {e}");
            } else {
                window.set_is_playing(true);
                window.set_playback_icon("⏸".into());
            }
        }
    }
}

fn prefetch_track_waveform(track: &Track, waveform_cache: &WaveformCacheMap) {
    let path = track.audio_path.clone();
    let stamp = track.mtime.unwrap_or(0.0) as u64;
    {
        if let Ok(cache) = waveform_cache.lock() {
            if cache.contains_key(&path) {
                return;
            }
        }
    }
    if let Some(store) = WaveformStore::default_location() {
        if let Some(bars) = store.get(&path, stamp) {
            if let Ok(mut cache) = waveform_cache.lock() {
                cache.insert(path, Arc::new(bars));
            }
            return;
        }
    }
    let wf_cache = Arc::clone(waveform_cache);
    thread::spawn(move || {
        let envelope = Arc::new(compute_audio_waveform(&path));
        if let Some(store) = WaveformStore::default_location() {
            store.put(&path, stamp, &envelope);
        }
        if let Ok(mut cache) = wf_cache.lock() {
            cache.insert(path, envelope);
        }
    });
}

fn prefetch_next_track(app_state: &Arc<Mutex<AppState>>, waveform_cache: &WaveformCacheMap) {
    let next_track = {
        let Ok(s) = app_state.lock() else { return };
        if s.filtered_tracks.is_empty() {
            return;
        }
        let next_row = match s.current_filtered_row {
            Some(cur) => (cur + 1) % s.filtered_tracks.len(),
            None => 0,
        };
        let track_idx = s.filtered_tracks[next_row];
        s.all_tracks.get(track_idx).cloned()
    };
    if let Some(track) = next_track {
        prefetch_track_waveform(&track, waveform_cache);
    }
}

fn play_next_track(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    engine: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &ArtLoader,
    waveform_cache: &WaveformCacheMap,
) {
    let next_track_opt = {
        let mut s = match app_state.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if s.filtered_tracks.is_empty() {
            return;
        }
        let total = s.filtered_tracks.len();
        let next_row = match s.current_filtered_row {
            Some(cur) => (cur + 1) % total,
            None => 0,
        };
        s.current_filtered_row = Some(next_row);
        let track_idx = s.filtered_tracks[next_row];
        s.all_tracks.get(track_idx).cloned()
    };

    if let Some(track) = next_track_opt {
        setup_track_for_playback(
            &track,
            window,
            engine,
            lyric_state,
            art_loader,
            waveform_cache,
            true,
        );
        prefetch_next_track(app_state, waveform_cache);
    }
}

fn play_prev_track(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    engine: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &ArtLoader,
    waveform_cache: &WaveformCacheMap,
) {
    let prev_track_opt = {
        let mut s = match app_state.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if s.filtered_tracks.is_empty() {
            return;
        }
        let total = s.filtered_tracks.len();
        let prev_row = match s.current_filtered_row {
            Some(cur) => {
                if cur == 0 {
                    total - 1
                } else {
                    cur - 1
                }
            }
            None => 0,
        };
        s.current_filtered_row = Some(prev_row);
        let track_idx = s.filtered_tracks[prev_row];
        s.all_tracks.get(track_idx).cloned()
    };

    if let Some(track) = prev_track_opt {
        setup_track_for_playback(
            &track,
            window,
            engine,
            lyric_state,
            art_loader,
            waveform_cache,
            true,
        );
        prefetch_next_track(app_state, waveform_cache);
    }
}

/// Plays row `row_idx` of the album or artist detail list. Both lists are built
/// from `all_tracks`, so the row index maps through the indices recorded when
/// the list was built.
#[allow(clippy::too_many_arguments)]
fn play_detail_row(
    window: &slint::Weak<MainWindow>,
    state: &Arc<Mutex<AppState>>,
    engine: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &ArtLoader,
    waveform_cache: &WaveformCacheMap,
    row_idx: i32,
    from_artist_view: bool,
) {
    let Some(w) = window.upgrade() else { return };
    let track = {
        let Ok(s) = state.lock() else { return };
        let indices = if from_artist_view {
            &s.artist_detail_tracks
        } else {
            &s.album_detail_tracks
        };
        indices
            .get(row_idx as usize)
            .and_then(|&i| s.all_tracks.get(i))
            .cloned()
    };
    if let Some(track) = track {
        setup_track_for_playback(
            &track,
            &w,
            engine,
            lyric_state,
            art_loader,
            waveform_cache,
            true,
        );
        prefetch_next_track(state, waveform_cache);
    }
}

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
    // 100ms playback position, reactive waveform, and auto-advance timer
    // -----------------------------------------------------------------------
    let timer_audio = Arc::clone(&audio);
    let timer_lyrics = Arc::clone(&lyric_state);
    let timer_app_state = Arc::clone(&app_state);
    let timer_art_loader = Arc::clone(&art_loader);
    let timer_wf_cache = Arc::clone(&waveform_cache);
    let timer_window = window.as_weak();
    // Smoothed loudness at the playhead, and the track it belongs to. Rising
    // quickly and falling slowly is what makes the bars breathe with the music
    // instead of flickering.
    let mut level = 0.0f32;
    let mut level_path: Option<String> = None;

    // 30 fps rather than 10: the waveform and the playhead are both drawn from
    // the position, and at 10 fps every step was visible as a jump. Everything
    // that only changes once a second is compared before it is written, so the
    // faster tick costs a handful of property reads.
    let position_timer = Timer::default();
    position_timer.start(TimerMode::Repeated, Duration::from_millis(33), move || {
        let Some(window) = timer_window.upgrade() else {
            return;
        };
        let Ok(engine) = timer_audio.lock() else {
            return;
        };
        let position_opt = engine.position_seconds();
        let duration = engine.duration_seconds().unwrap_or(0.0);
        let playing = engine.is_playing();
        let current_path = engine.current_path.clone();
        let raw_live_level = engine.live_level.get();
        drop(engine);

        let Some(position) = position_opt else {
            return;
        };

        let progress = if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        let position_text = format_time(position);
        if window.get_position_text().as_str() != position_text.as_str() {
            window.set_position_text(position_text.into());
        }
        let duration_text = format_time(duration);
        if window.get_duration_text().as_str() != duration_text.as_str() {
            window.set_duration_text(duration_text.into());
        }
        if window.get_is_playing() != playing {
            window.set_is_playing(playing);
            window.set_playback_icon(if playing { "⏸" } else { "▶" }.into());
        }
        if (window.get_seek_position() - progress).abs() > 0.0005 {
            window.set_seek_position(progress);
        }

        // Sound-reactive waveform. The static shape comes from the track's
        // envelope (decoded once per track and cached on disk), but the *level*
        // that drives the animation is the real live RMS amplitude from the audio
        // thread — captured by the LevelCapture source wrapper.
        // If the track's exact envelope is still computing, use default_waveform()
        // as the baseline so the waveform starts reacting and pulsing to the music
        // immediately from frame 1 rather than freezing flat!
        let envelope = current_path.as_ref().and_then(|path| {
            let cache = timer_wf_cache.lock().ok()?;
            cache.get(path).cloned()
        });
        let default_env = default_waveform();
        let env_slice = envelope
            .as_deref()
            .map(|v| v.as_slice())
            .unwrap_or(&default_env);
        if level_path.as_deref() != current_path.as_deref() {
            level_path = current_path.clone();
            level = 0.0;
        }
        let target = if playing { raw_live_level } else { 0.0 };
        // Smooth: fast attack so beats land immediately, moderate release
        // so bars don't flicker between frames.
        let rate = if target > level { 0.7 } else { 0.2 };
        level += (target - level) * rate;
        set_waveform_bars(&window, &reactive_bars(env_slice, progress, level, playing));

        // Update active lyric line and words using binary search and per-track offset
        if let Ok(lines) = timer_lyrics.lock() {
            let offset_ms = window.get_lyrics_offset_ms() as i64;
            let effective_position = (position + (offset_ms as f64 / 1000.0)).max(0.0);

            let active_idx = find_active_line_index(&lines, effective_position);
            if window.get_active_line_index() != active_idx {
                window.set_active_line_index(active_idx);
            }

            if active_idx >= 0 && (active_idx as usize) < lines.len() {
                let line = &lines[active_idx as usize];
                if !line.words.is_empty() {
                    let cur_word_idx = find_active_word_index(&line.words, effective_position);
                    update_ui_words(&window, &line.words, cur_word_idx, effective_position);
                } else if window.get_active_has_words() {
                    window.set_active_has_words(false);
                }
            } else if window.get_active_has_words() {
                window.set_active_has_words(false);
            }
        }

        // Detect end of track and auto advance to next song in playlist
        let should_advance = {
            if let Ok(mut s) = timer_app_state.lock() {
                let finished = s.was_playing
                    && !playing
                    && (duration > 0.0 && position >= duration - 1.2 || position < 0.05);
                s.was_playing = playing;
                finished
            } else {
                false
            }
        };

        if should_advance {
            play_next_track(
                &window,
                &timer_app_state,
                &timer_audio,
                &timer_lyrics,
                &timer_art_loader,
                &timer_wf_cache,
            );
        }
    });

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
    // Batch Download: Options dialog, incremental rescan, live progress & platform logs
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        window.on_download_all(move || {
            let Some(w) = weak.upgrade() else { return };
            // If already running or finished in stage 1, restore dialog directly:
            if w.get_batch_is_running()
                || (w.get_batch_is_finished() && w.get_batch_download_stage() == 1)
            {
                w.set_batch_download_open(true);
                return;
            }
            let Ok(s) = state.lock() else { return };
            let total = s.all_tracks.len() as i32;
            let missing = s
                .all_tracks
                .iter()
                .filter(|t| t.status == LyricsStatus::Missing)
                .count() as i32;
            let plain = s
                .all_tracks
                .iter()
                .filter(|t| t.status == LyricsStatus::Plain)
                .count() as i32;
            let suspicious = s
                .all_tracks
                .iter()
                .filter(|t| t.status == LyricsStatus::Suspicious)
                .count() as i32;
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
        let _job_ctrl = Arc::clone(&batch_job_control);
        window.on_batch_close(move || {
            if let Some(w) = weak.upgrade() {
                w.set_batch_download_open(false);
                // When closing the modal with 'X' while running, do not cancel the job.
                // The download continues running in the background and can be restored
                // from the floating pill or the toolbar Download button.
                // Cancellation is explicitly handled by on_batch_stop ("Stop" button).
                if w.get_batch_is_finished() {
                    w.set_batch_is_finished(false);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let job_ctrl = Arc::clone(&batch_job_control);
        window.on_batch_pause_resume(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(lock) = job_ctrl.lock() {
                if let Some(ref job) = *lock {
                    let next = !job.paused.load(std::sync::atomic::Ordering::Relaxed);
                    job.paused.store(next, std::sync::atomic::Ordering::Relaxed);
                    w.set_batch_is_paused(next);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let job_ctrl = Arc::clone(&batch_job_control);
        window.on_batch_stop(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(mut lock) = job_ctrl.lock() {
                if let Some(ref job) = *lock {
                    job.cancelled
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    job.paused
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                }
                *lock = None;
            }
            w.set_batch_is_running(false);
            w.set_batch_phase_text(SharedString::from("Stopped by user."));
            w.set_batch_is_finished(true);
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        let job_ctrl = Arc::clone(&batch_job_control);
        window.on_batch_start_mode(move |mode_idx| {
            let Some(w) = weak.upgrade() else { return };
            let (mode, mode_title) = match mode_idx {
                0 => (DownloadMode::ReplaceAll, "Batch Download: Whole Directory"),
                1 => (DownloadMode::MissingOnly, "Batch Download: Missing Only"),
                2 => (
                    DownloadMode::UpgradePlain,
                    "Batch Download: Upgrade Plain to Synced",
                ),
                3 => (
                    DownloadMode::FixSuspicious,
                    "Batch Download: Fix Suspicious",
                ),
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
            w.set_batch_phase_text(SharedString::from(
                "Phase 1: Rescanning music directories...",
            ));
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
                details: "Rescanning music folders to update tracks before starting download..."
                    .into(),
                action_text: "".into(),
                badge_text: "SCAN".into(),
                badge_type: "scan".into(),
            }];
            w.set_batch_logs(ModelRc::new(VecModel::from(initial_logs.clone())));

            w.set_batch_download_stage(1);
            w.set_batch_is_running(true);
            w.set_batch_is_paused(false);
            w.set_batch_is_finished(false);
            w.set_batch_phase_text("Phase 1: Scanning music library...".into());

            let event_loop = w.as_weak();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            let job_ctrl = Arc::clone(&job_ctrl);
            let cancelled = Arc::clone(&cancelled);
            let paused = Arc::clone(&paused);

            thread::spawn(move || {
                let mut logs = initial_logs;

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

                // Phase 1: Rescan music directories
                let dirs = db.directories().unwrap_or_default();
                let existing_map = db.get_tracks_map().unwrap_or_default();
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

                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }

                let scan_res = lyrics_core::scan_audio_paths_incremental(
                    all_paths,
                    Some(&existing_map),
                    |_, _| {},
                );
                if !scan_res.changed_tracks.is_empty() {
                    let _ = db.upsert_tracks(&scan_res.changed_tracks);
                }

                // Reload tracks into app_state and update main UI
                let el = event_loop.clone();
                let st = Arc::clone(&state);
                let ac = Arc::clone(&art_loader);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el.upgrade() {
                        ac.reset_after_scan();
                        load_library_into_ui(&w, &st, &ac);
                    }
                });

                let all_tracks = db.tracks().unwrap_or_default();

                logs.push(BatchLogEntry {
                    timestamp: current_timestamp().into(),
                    track_title: "Rescan Complete".into(),
                    track_artist: "Library".into(),
                    details: format!("Discovered {} total tracks in library.", all_tracks.len())
                        .into(),
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

                // Phase 2: Select candidate tracks based on chosen mode
                let candidates: Vec<Track> = all_tracks
                    .into_iter()
                    .filter(|t| t.should_download(mode))
                    .collect();

                let total_candidates = candidates.len();
                if total_candidates == 0 {
                    logs.push(BatchLogEntry {
                        timestamp: current_timestamp().into(),
                        track_title: "No Matching Songs".into(),
                        track_artist: "Batch".into(),
                        details:
                            "No tracks matching this download criteria were found in your library."
                                .into(),
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
                let mut stats = BatchStats {
                    total: total_candidates as i32,
                    processed: 0,
                    remaining: total_candidates as i32,
                    synced: 0,
                    plain: 0,
                    not_found: 0,
                    errors: 0,
                };

                let el = event_loop.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el.upgrade() {
                        w.set_batch_phase_text(
                            format!("Phase 2: Downloading lyrics ({} tracks)", total_candidates)
                                .into(),
                        );
                    }
                });

                // Phase 3: Download loop
                let mut was_cancelled = false;
                for (i, track) in candidates.iter().enumerate() {
                    // Check cancellation
                    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                        was_cancelled = true;
                        break;
                    }

                    // Handle pause
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
                    let prog_text = format!(
                        "Song {} of {} ({} remaining)",
                        i + 1,
                        total_candidates,
                        total_candidates - i
                    );

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

                    // Search providers and track results per platform
                    let provider_results = Arc::new(Mutex::new(Vec::<(
                        ProviderName,
                        Option<bool>,
                        Option<String>,
                    )>::new()));
                    let outcome_res = match providers_for_settings(&db, track, &settings) {
                        Ok(providers) => {
                            let pr_cb = Arc::clone(&provider_results);
                            download_track(track, mode, &providers, &cancelled, move |event| {
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

                    let pr_list = provider_results
                        .lock()
                        .map(|l| l.clone())
                        .unwrap_or_default();
                    let mut parts = Vec::new();
                    for (p, status, err) in pr_list {
                        if let Some(synced) = status {
                            parts.push(format!(
                                "✓ {} ({})",
                                p.label(),
                                if synced { "synced" } else { "plain" }
                            ));
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
                                let _ = db.update_lyrics_status(
                                    &track.audio_path,
                                    LyricsStatus::Synced,
                                    Some(provider.label()),
                                );
                                (
                                    "SYNCED",
                                    "synced",
                                    format!("→ Saved synced lyrics from {}", provider.label()),
                                )
                            } else {
                                stats.plain += 1;
                                let _ = db.update_lyrics_status(
                                    &track.audio_path,
                                    LyricsStatus::Plain,
                                    Some(provider.label()),
                                );
                                (
                                    "PLAIN",
                                    "plain",
                                    format!("→ Saved plain lyrics from {}", provider.label()),
                                )
                            }
                        }
                        Ok(DownloadOutcome::NotFound) => {
                            stats.not_found += 1;
                            (
                                "NOT FOUND",
                                "missing",
                                "→ No matching lyrics found".to_string(),
                            )
                        }
                        Ok(DownloadOutcome::Skipped) => (
                            "SKIPPED",
                            "info",
                            "→ Skipped (kept existing lyrics)".to_string(),
                        ),
                        Ok(DownloadOutcome::Cancelled) => {
                            was_cancelled = true;
                            ("CANCELLED", "missing", "→ Cancelled by user".to_string())
                        }
                        Ok(DownloadOutcome::DryRun { .. }) => (
                            "DRY RUN",
                            "info",
                            "→ Dry run (no lyrics written)".to_string(),
                        ),
                        Err(err) => {
                            stats.errors += 1;
                            ("ERROR", "error", format!("→ Error: {err}"))
                        }
                    };

                    stats.processed = (i + 1) as i32;
                    stats.remaining = (total_candidates - (i + 1)) as i32;

                    logs.push(BatchLogEntry {
                        timestamp: current_timestamp().into(),
                        track_title: track.title.clone().into(),
                        track_artist: track.artist.clone().into(),
                        details: providers_summary.into(),
                        action_text: result_action.into(),
                        badge_text: badge_text.into(),
                        badge_type: badge_type.into(),
                    });

                    // Update UI live
                    let el = event_loop.clone();
                    let logs_clone = logs.clone();
                    let stats_clone = stats.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = el.upgrade() {
                            w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
                            w.set_batch_stats(stats_clone);
                        }
                    });

                    // Periodically refresh library view in background so track list shows live badges
                    if (i + 1) % 5 == 0 || i + 1 == total_candidates {
                        let el = event_loop.clone();
                        let st = Arc::clone(&state);
                        let ac = Arc::clone(&art_loader);
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

                // Phase 4: Finish
                logs.push(BatchLogEntry {
                    timestamp: current_timestamp().into(),
                    track_title: if was_cancelled {
                        "Process Stopped"
                    } else {
                        "Process Finished"
                    }
                    .into(),
                    track_artist: "Batch".into(),
                    details: format!(
                        "Processed {}/{} tracks. Synced: {}, Plain: {}, Not Found: {}, Errors: {}",
                        stats.processed,
                        total_candidates,
                        stats.synced,
                        stats.plain,
                        stats.not_found,
                        stats.errors
                    )
                    .into(),
                    action_text: "".into(),
                    badge_text: if was_cancelled { "STOPPED" } else { "DONE" }.into(),
                    badge_type: if was_cancelled { "missing" } else { "synced" }.into(),
                });

                if let Ok(mut lock) = job_ctrl.lock() {
                    *lock = None;
                }

                let el = event_loop.clone();
                let logs_clone = logs.clone();
                let stats_clone = stats.clone();
                let st = Arc::clone(&state);
                let ac = Arc::clone(&art_loader);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el.upgrade() {
                        w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
                        w.set_batch_stats(stats_clone);
                        w.set_batch_progress(1.0);
                        w.set_batch_progress_text(
                            format!("{}/{}", stats.processed, total_candidates).into(),
                        );
                        w.set_batch_current_track(
                            if was_cancelled {
                                "Batch download stopped."
                            } else {
                                "Batch download completed!"
                            }
                            .into(),
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
            });
        });
    }

    // -----------------------------------------------------------------------
    // Library Health View: Audits, safe fixes, preview diff, result logs & crash resumption
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        window.on_health_re_audit(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(s) = state.lock() {
                load_health_into_ui(&w, &s.all_tracks);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
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
        let state = Arc::clone(&app_state);
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
        let state = Arc::clone(&app_state);
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
            let Ok(settings) = load_settings() else {
                return;
            };

            let event_loop = w.as_weak();
            thread::spawn(move || {
                let mut preview_items = Vec::new();

                for issue in &safe_issues {
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
                        preview_items.push(PreviewDiffItem {
                            path: issue.audio_path.clone().into(),
                            title: issue.title.clone().into(),
                            artist: issue.artist.clone().into(),
                            issue_category: issue.category.display_label().into(),
                            action_label: format!(
                                "Adjust offset ({:+.2}s)",
                                offset_ms as f64 / 1000.0
                            )
                            .into(),
                            provider: "Internal (Offset)".into(),
                            score_text: "100%".into(),
                            selection_reason: "Detected timestamp offset correction".into(),
                            diff_text: diff_text.into(),
                        });
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

                        let candidates = providers_for_settings(&db, &fake_track, &settings)
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

                        if let Some(best) = candidates.first() {
                            let new_text = best.candidate.best_text().unwrap_or_default();
                            let diff_text = generate_unified_diff(
                                &old_text,
                                new_text,
                                if old_text.is_empty() {
                                    "Missing"
                                } else {
                                    "Current Lyrics"
                                },
                                &format!("Proposed ({})", best.candidate.provider.label()),
                            );
                            preview_items.push(PreviewDiffItem {
                                path: issue.audio_path.clone().into(),
                                title: issue.title.clone().into(),
                                artist: issue.artist.clone().into(),
                                issue_category: issue.category.display_label().into(),
                                action_label: if best.is_synced {
                                    "Download Synced".into()
                                } else {
                                    "Download Plain".into()
                                },
                                provider: best.candidate.provider.label().into(),
                                score_text: format!("{:.0}%", best.rank_score * 100.0).into(),
                                selection_reason: best
                                    .selection_reason
                                    .clone()
                                    .unwrap_or_else(|| "Highest score match".to_string())
                                    .into(),
                                diff_text: diff_text.into(),
                            });
                        }
                    }
                }

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
        let state = Arc::clone(&app_state);
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

            let Some(issue) = filtered_issues.get(idx as usize).cloned() else {
                return;
            };

            let Ok(db) = open_database() else { return };
            let Ok(settings) = load_settings() else {
                return;
            };

            let event_loop = w.as_weak();
            thread::spawn(move || {
                let mut preview_items = Vec::new();
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
                    preview_items.push(PreviewDiffItem {
                        path: issue.audio_path.clone().into(),
                        title: issue.title.clone().into(),
                        artist: issue.artist.clone().into(),
                        issue_category: issue.category.display_label().into(),
                        action_label: format!("Adjust offset ({:+.2}s)", offset_ms as f64 / 1000.0)
                            .into(),
                        provider: "Internal (Offset)".into(),
                        score_text: "100%".into(),
                        selection_reason: "Detected timestamp offset correction".into(),
                        diff_text: diff_text.into(),
                    });
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

                    let candidates = providers_for_settings(&db, &fake_track, &settings)
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

                    let (diff_text, provider_name, score_text, reason) =
                        if let Some(best) = candidates.first() {
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

                    preview_items.push(PreviewDiffItem {
                        path: issue.audio_path.clone().into(),
                        title: issue.title.clone().into(),
                        artist: issue.artist.clone().into(),
                        issue_category: issue.category.display_label().into(),
                        action_label: "Inspect / Fix".into(),
                        provider: provider_name.into(),
                        score_text: score_text.into(),
                        selection_reason: reason.into(),
                        diff_text: diff_text.into(),
                    });
                }

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
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let art = Arc::clone(&art_loader);
        let job_ctrl = Arc::clone(&health_job_control);
        window.on_health_apply_fixes(move || {
            let Some(w) = weak.upgrade() else { return };
            w.set_health_preview_modal_open(false);

            let preview_items: Vec<PreviewDiffItem> = {
                let model = w.get_health_preview_items();
                let count = model.row_count();
                (0..count).filter_map(|i| model.row_data(i)).collect()
            };

            if preview_items.is_empty() {
                return;
            }

            let Ok(db) = open_database() else { return };
            let Ok(settings) = load_settings() else {
                return;
            };
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

            let Ok(job_id) = db.create_batch_job("fix_safe_issues", "safe_issues", &drafts) else {
                return;
            };

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

            let event_loop = w.as_weak();
            let st = Arc::clone(&state);
            let ac = Arc::clone(&art);
            thread::spawn(move || {
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
                    let prog_text =
                        format!("{}/{} ({} remaining)", idx + 1, total, total - (idx + 1));
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
                            let _ = db.update_lyrics_status(
                                &audio_path,
                                LyricsStatus::Synced,
                                Some("Internal"),
                            );
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
                                download_track_with_options(
                                    &track,
                                    mode,
                                    &providers,
                                    &cancelled,
                                    dl_opts,
                                    |_| {},
                                )
                            {
                                chosen_provider = provider.label().to_string();
                                let status = if synced {
                                    LyricsStatus::Synced
                                } else {
                                    LyricsStatus::Plain
                                };
                                let _ = db.update_lyrics_status(
                                    &audio_path,
                                    status,
                                    Some(provider.label()),
                                );
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

                let final_status = if was_cancelled {
                    "cancelled"
                } else {
                    "completed"
                };
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
                        load_library_into_ui(&w, &st, &ac);
                    }
                });
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let art = Arc::clone(&art_loader);
        let job_ctrl = Arc::clone(&health_job_control);
        window.on_health_resume_batch(move || {
            let Some(w) = weak.upgrade() else { return };
            let Ok(db) = open_database() else { return };
            let Ok(Some(job)) = db.get_resumable_batch_job() else {
                return;
            };
            let Ok(pending_items) = db.get_pending_batch_job_items(job.id) else {
                return;
            };
            if pending_items.is_empty() {
                let _ = db.update_batch_job_status(job.id, "completed");
                w.set_health_has_resumable_job(false);
                return;
            }

            let Ok(settings) = load_settings() else {
                return;
            };
            let backup_enabled = w.get_settings_backup_before_overwrite();

            let cancelled = Arc::new(AtomicBool::new(false));
            let paused = Arc::new(AtomicBool::new(false));
            if let Ok(mut lock) = job_ctrl.lock() {
                *lock = Some(ActiveBatchJob {
                    cancelled: Arc::clone(&cancelled),
                    paused: Arc::clone(&paused),
                });
            }

            let job_id = job.id;
            let total = job.total_tracks;
            let already_completed = job.completed_tracks;
            w.set_health_has_resumable_job(false);
            w.set_health_is_running(true);
            w.set_health_is_paused(false);
            w.set_health_progress(already_completed as f32 / total as f32);
            w.set_health_progress_text(format!("{} / {}", already_completed, total).into());

            let event_loop = w.as_weak();
            let st = Arc::clone(&state);
            let ac = Arc::clone(&art);
            thread::spawn(move || {
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
                    let prog_text = format!(
                        "{}/{} ({} remaining)",
                        cur_idx,
                        total,
                        total.saturating_sub(cur_idx)
                    );
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = el.upgrade() {
                            w.set_health_current_track_text(cur_title.into());
                            w.set_health_progress(prog);
                            w.set_health_progress_text(prog_text.into());
                        }
                    });

                    let audio_path = item.audio_path.clone();
                    let lrc_path = PathBuf::from(&item.lrc_path);

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

                    let mut chosen_provider = item
                        .provider
                        .clone()
                        .unwrap_or_else(|| "LRCLIB".to_string());
                    let mut chosen_score = item.score.unwrap_or(0.95);
                    let chosen_reason = item
                        .selection_reason
                        .clone()
                        .unwrap_or_else(|| "Resumed batch fix".to_string());
                    let mut is_success = false;

                    if item
                        .issue_reason
                        .as_deref()
                        .unwrap_or("")
                        .starts_with("Offset")
                    {
                        let old_text = fs::read_to_string(&lrc_path).unwrap_or_default();
                        let offset_ms =
                            parse_offset_from_reason(item.issue_reason.as_deref().unwrap_or(""))
                                .unwrap_or(0);
                        let new_text = apply_offset_to_lrc(&old_text, offset_ms);
                        if write_atomic_with_backup(&lrc_path, &new_text, backup_enabled).is_ok() {
                            let _ = db.update_lyrics_status(
                                &audio_path,
                                LyricsStatus::Synced,
                                Some("Internal"),
                            );
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
                                download_track_with_options(
                                    &track,
                                    DownloadMode::SmartUpdate,
                                    &providers,
                                    &cancelled,
                                    dl_opts,
                                    |_| {},
                                )
                            {
                                chosen_provider = provider.label().to_string();
                                let status = if synced {
                                    LyricsStatus::Synced
                                } else {
                                    LyricsStatus::Plain
                                };
                                let _ = db.update_lyrics_status(
                                    &audio_path,
                                    status,
                                    Some(provider.label()),
                                );
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
                        None,
                        None,
                    );
                }

                let final_status = if was_cancelled {
                    "cancelled"
                } else {
                    "completed"
                };
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
                        load_library_into_ui(&w, &st, &ac);
                    }
                });
            });
        });
    }
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
        let job_ctrl = Arc::clone(&health_job_control);
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
        let job_ctrl = Arc::clone(&health_job_control);
        window.on_health_stop_batch(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(mut lock) = job_ctrl.lock() {
                if let Some(ref job) = *lock {
                    job.cancelled
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    job.paused
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                }
                *lock = None;
            }
            w.set_health_is_running(false);
            w.set_health_current_track_text("Batch fix stopped by user.".into());
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

    // -----------------------------------------------------------------------
    // Search and filter
    // -----------------------------------------------------------------------
    wire_search_and_filters(&window, &app_state, &art_loader);

    // -----------------------------------------------------------------------
    // Track row click: select *and* play, so one click starts the song.
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let lyrics = Arc::clone(&lyric_state);
        let engine = Arc::clone(&audio);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_select_track(move |row_idx| {
            let Some(w) = weak.upgrade() else { return };
            let Ok(mut s) = state.lock() else { return };
            let row = row_idx as usize;
            let Some(&track_idx) = s.filtered_tracks.get(row) else {
                return;
            };
            s.current_filtered_row = Some(row);
            let Some(track) = s.all_tracks.get(track_idx).cloned() else {
                return;
            };
            drop(s);
            setup_track_for_playback(&track, &w, &engine, &lyrics, &cache, &wf_cache, true);
            prefetch_next_track(&state, &wf_cache);
        });
    }

    // -----------------------------------------------------------------------
    // Play track (play button on row)
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let engine = Arc::clone(&audio);
        let lyrics = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_play_track(move |row_idx| {
            let Some(w) = weak.upgrade() else { return };
            let Ok(mut s) = state.lock() else { return };
            let row = row_idx as usize;
            let Some(&track_idx) = s.filtered_tracks.get(row) else {
                return;
            };
            s.current_filtered_row = Some(row);
            let Some(track) = s.all_tracks.get(track_idx).cloned() else {
                return;
            };
            drop(s);
            setup_track_for_playback(&track, &w, &engine, &lyrics, &cache, &wf_cache, true);
            prefetch_next_track(&state, &wf_cache);
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let engine = Arc::clone(&audio);
        let lyrics = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_play_queue_track(move |row_idx| {
            let Some(w) = weak.upgrade() else { return };
            let Ok(mut s) = state.lock() else { return };
            let row = row_idx as usize;
            let Some(&track_idx) = s.filtered_tracks.get(row) else {
                return;
            };
            s.current_filtered_row = Some(row);
            let Some(track) = s.all_tracks.get(track_idx).cloned() else {
                return;
            };
            drop(s);
            setup_track_for_playback(&track, &w, &engine, &lyrics, &cache, &wf_cache, true);
            prefetch_next_track(&state, &wf_cache);
        });
    }

    // -----------------------------------------------------------------------
    // Player bar controls
    // -----------------------------------------------------------------------
    {
        let engine = Arc::clone(&audio);
        let weak = window.as_weak();
        window.on_play_pause(move || {
            if let Ok(mut eng) = engine.lock() {
                match eng.toggle_pause() {
                    Ok(paused) => {
                        if let Some(w) = weak.upgrade() {
                            w.set_is_playing(!paused);
                            w.set_playback_icon(if paused { "▶" } else { "⏸" }.into());
                            if !paused && w.get_lyrics_lines().row_count() > 0 {
                                w.set_lyrics_visible(true);
                            }
                        }
                    }
                    Err(e) => eprintln!("Play/Pause toggle error: {e}"),
                }
            }
        });
    }
    {
        let engine = Arc::clone(&audio);
        let lyrics = Arc::clone(&lyric_state);
        let weak = window.as_weak();
        window.on_seek(move |fraction| {
            let target_opt = if let Ok(mut eng) = engine.lock() {
                if let Some(dur) = eng.duration_seconds() {
                    let target_pos = fraction as f64 * dur;
                    let _ = eng.seek(target_pos);
                    Some(target_pos)
                } else {
                    None
                }
            } else {
                None
            };
            if let (Some(target_pos), Some(w)) = (target_opt, weak.upgrade()) {
                let offset_ms = w.get_lyrics_offset_ms() as i64;
                let effective_pos = (target_pos + (offset_ms as f64 / 1000.0)).max(0.0);
                if let Ok(lines) = lyrics.lock() {
                    let active_idx = find_active_line_index(&lines, effective_pos);
                    w.set_active_line_index(active_idx);
                    if active_idx >= 0 && (active_idx as usize) < lines.len() {
                        let line = &lines[active_idx as usize];
                        if !line.words.is_empty() {
                            let cur_word = find_active_word_index(&line.words, effective_pos);
                            update_ui_words(&w, &line.words, cur_word, effective_pos);
                        } else if w.get_active_has_words() {
                            w.set_active_has_words(false);
                        }
                    } else if w.get_active_has_words() {
                        w.set_active_has_words(false);
                    }
                }
            }
        });
    }
    {
        let engine = Arc::clone(&audio);
        window.on_rewind(move || {
            if let Ok(mut eng) = engine.lock() {
                if let Some(pos) = eng.position_seconds() {
                    let _ = eng.seek((pos - 10.0).max(0.0));
                }
            }
        });
    }
    {
        let engine = Arc::clone(&audio);
        window.on_forward(move || {
            if let Ok(mut eng) = engine.lock() {
                if let Some(pos) = eng.position_seconds() {
                    let target = pos + 10.0;
                    let _ = eng.seek(target);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        let lyr = Arc::clone(&lyric_state);
        let art = Arc::clone(&art_loader);
        let wf = Arc::clone(&waveform_cache);
        window.on_next_track(move || {
            if let Some(w) = weak.upgrade() {
                play_next_track(&w, &state, &audio_eng, &lyr, &art, &wf);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        let lyr = Arc::clone(&lyric_state);
        let art = Arc::clone(&art_loader);
        let wf = Arc::clone(&waveform_cache);
        window.on_prev_track(move || {
            if let Some(w) = weak.upgrade() {
                play_prev_track(&w, &state, &audio_eng, &lyr, &art, &wf);
            }
        });
    }
    {
        let engine = Arc::clone(&audio);
        window.on_volume_changed(move |vol| {
            if let Ok(mut eng) = engine.lock() {
                eng.set_volume(vol);
            }
        });
    }

    // -----------------------------------------------------------------------
    // Lyrics: seek to line & copy
    // -----------------------------------------------------------------------
    {
        let lyrics = Arc::clone(&lyric_state);
        let engine = Arc::clone(&audio);
        let weak = window.as_weak();
        window.on_seek_to_line(move |line_idx| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let offset_ms = w.get_lyrics_offset_ms() as i64;
            if let (Ok(lines), Ok(mut eng)) = (lyrics.lock(), engine.lock()) {
                if let Some(line) = lines.get(line_idx as usize) {
                    let target = (line.timestamp_seconds - (offset_ms as f64 / 1000.0)).max(0.0);
                    let _ = eng.seek(target);
                    w.set_active_line_index(line_idx);
                    if !line.words.is_empty() {
                        let effective_pos = (target + (offset_ms as f64 / 1000.0)).max(0.0);
                        let cur_word = find_active_word_index(&line.words, effective_pos);
                        update_ui_words(&w, &line.words, cur_word, effective_pos);
                    } else if w.get_active_has_words() {
                        w.set_active_has_words(false);
                    }
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // Per-track offset control: adjust, set, write-to-lrc
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        window.on_adjust_lyrics_offset(move |delta_ms| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let current = w.get_lyrics_offset_ms();
            let new_offset = current + delta_ms;
            w.set_lyrics_offset_ms(new_offset);
            w.set_lyrics_offset_save_status("".into());

            // Save to DB immediately
            if let Ok(audio) = audio_eng.lock() {
                if let Some(path) = &audio.current_path {
                    if let Ok(db) = open_database() {
                        let _ = db.update_track_offset(path, new_offset as i64);
                    }
                    if let Ok(mut s) = state.lock() {
                        if let Some(t) = s.all_tracks.iter_mut().find(|t| t.audio_path == *path) {
                            t.lyrics_offset_ms = new_offset as i64;
                        }
                    }
                }
            }
        });
    }

    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        window.on_set_lyrics_offset(move |target_ms| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            w.set_lyrics_offset_ms(target_ms);
            w.set_lyrics_offset_save_status("".into());

            if let Ok(audio) = audio_eng.lock() {
                if let Some(path) = &audio.current_path {
                    if let Ok(db) = open_database() {
                        let _ = db.update_track_offset(path, target_ms as i64);
                    }
                    if let Ok(mut s) = state.lock() {
                        if let Some(t) = s.all_tracks.iter_mut().find(|t| t.audio_path == *path) {
                            t.lyrics_offset_ms = target_ms as i64;
                        }
                    }
                }
            }
        });
    }

    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        let lyric_st = Arc::clone(&lyric_state);
        window.on_write_lyrics_offset_to_file(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let offset_ms = w.get_lyrics_offset_ms() as i64;
            if offset_ms == 0 {
                return;
            }

            let path_opt = audio_eng.lock().ok().and_then(|a| a.current_path.clone());
            let Some(path) = path_opt else {
                return;
            };

            let track_opt = state
                .lock()
                .ok()
                .and_then(|s| s.all_tracks.iter().find(|t| t.audio_path == path).cloned());
            let Some(track) = track_opt else {
                return;
            };

            if let Ok(content) = fs::read_to_string(&track.lrc_path) {
                let offset_sec = offset_ms as f64 / 1000.0;
                let updated = lyrics_core::apply_offset_tag(&content, offset_sec);
                if fs::write(&track.lrc_path, &updated).is_ok() {
                    // Reset DB offset to 0 since it is now baked into the file
                    if let Ok(db) = open_database() {
                        let _ = db.update_track_offset(&path, 0);
                    }
                    if let Ok(mut s) = state.lock() {
                        if let Some(t) = s.all_tracks.iter_mut().find(|t| t.audio_path == path) {
                            t.lyrics_offset_ms = 0;
                        }
                    }
                    w.set_lyrics_offset_ms(0);
                    w.set_lyrics_offset_save_status("Saved to .lrc".into());

                    // Reload lines
                    let reloaded = lyrics_core::parse_lines(&updated);
                    let ui_lines: Vec<LyricLine> = reloaded
                        .iter()
                        .map(|l| LyricLine {
                            timestamp: l.timestamp_seconds as f32,
                            text: l.text.clone().into(),
                        })
                        .collect();
                    w.set_lyrics_lines(ModelRc::new(VecModel::from(ui_lines)));
                    if let Ok(mut ls) = lyric_st.lock() {
                        *ls = reloaded;
                    }
                }
            }
        });
    }
    {
        let lyrics = Arc::clone(&lyric_state);
        window.on_copy_lyrics(move || {
            if let Ok(lines) = lyrics.lock() {
                let text: String = lines
                    .iter()
                    .map(|l| l.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = text;
            }
        });
    }

    // -----------------------------------------------------------------------
    // Fetch single track lyrics
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        window.on_fetch_track(move |row_idx| {
            let Ok(s) = state.lock() else { return };
            let row = row_idx as usize;
            let Some(&track_idx) = s.filtered_tracks.get(row) else {
                return;
            };
            let Some(track) = s.all_tracks.get(track_idx).cloned() else {
                return;
            };
            drop(s);
            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let _ = (|| -> Result<(), String> {
                    let db = open_database()?;
                    let settings = load_settings().map_err(|e| e.to_string())?;
                    let providers = providers_for_settings(&db, &track, &settings)?;
                    let cancelled = AtomicBool::new(false);
                    let outcome = download_track(
                        &track,
                        DownloadMode::ReplaceAll,
                        &providers,
                        &cancelled,
                        |_| {},
                    )
                    .map_err(|e| e.to_string())?;
                    if let DownloadOutcome::Downloaded { provider, synced } = outcome {
                        db.update_lyrics_status(
                            &track.audio_path,
                            if synced {
                                LyricsStatus::Synced
                            } else {
                                LyricsStatus::Plain
                            },
                            Some(provider.label()),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = event_loop.upgrade() {
                            load_library_into_ui(&w, &state, &art_loader);
                        }
                    });
                    Ok(())
                })();
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let engine = Arc::clone(&audio);
        let cache = Arc::clone(&art_loader);
        window.on_fetch_current_lyrics(move || {
            let Ok(eng) = engine.lock() else { return };
            let Some(current_path) = eng.current_path.clone() else {
                return;
            };
            drop(eng);
            let Ok(s) = state.lock() else { return };
            let Some(track) = s
                .all_tracks
                .iter()
                .find(|t| t.audio_path == current_path)
                .cloned()
            else {
                return;
            };
            drop(s);
            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let _ = (|| -> Result<(), String> {
                    let db = open_database()?;
                    let settings = load_settings().map_err(|e| e.to_string())?;
                    let providers = providers_for_settings(&db, &track, &settings)?;
                    let cancelled = AtomicBool::new(false);
                    let outcome = download_track(
                        &track,
                        DownloadMode::ReplaceAll,
                        &providers,
                        &cancelled,
                        |_| {},
                    )
                    .map_err(|e| e.to_string())?;
                    if let DownloadOutcome::Downloaded { provider, synced } = outcome {
                        db.update_lyrics_status(
                            &track.audio_path,
                            if synced {
                                LyricsStatus::Synced
                            } else {
                                LyricsStatus::Plain
                            },
                            Some(provider.label()),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = event_loop.upgrade() {
                            load_library_into_ui(&w, &state, &art_loader);
                        }
                    });
                    Ok(())
                })();
            });
        });
    }

    // -----------------------------------------------------------------------
    // Suspicious lyrics actions: Fix offset (tag / rewrite) & Replace lyrics
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        let lyric_st = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_fix_offset_tag(move || {
            let Some(w) = weak.upgrade() else { return };
            let offset_sec = w.get_lyrics_estimated_offset() as f64;
            let current_path = audio_eng.lock().ok().and_then(|e| e.current_path.clone());
            let Ok(mut s) = state.lock() else { return };
            let idx_opt = s
                .current_filtered_row
                .and_then(|row| s.filtered_tracks.get(row).copied());
            let track_opt = if let Some(ref path) = current_path {
                s.all_tracks.iter_mut().find(|t| &t.audio_path == path)
            } else if let Some(idx) = idx_opt {
                s.all_tracks.get_mut(idx)
            } else {
                None
            };
            let Some(track) = track_opt else { return };

            if let Ok(contents) = fs::read_to_string(&track.lrc_path) {
                let fixed = lyrics_core::apply_offset_tag(&contents, offset_sec);
                if fs::write(&track.lrc_path, &fixed).is_ok() {
                    track.status = LyricsStatus::Synced;
                    track.suspicious_reason = None;
                    if let Ok(db) = open_database() {
                        let _ =
                            db.update_lyrics_status(&track.audio_path, LyricsStatus::Synced, None);
                    }
                    let track_clone = track.clone();
                    drop(s);
                    setup_track_for_playback(
                        &track_clone,
                        &w,
                        &audio_eng,
                        &lyric_st,
                        &cache,
                        &wf_cache,
                        false,
                    );
                    load_library_into_ui(&w, &state, &cache);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        let lyric_st = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_fix_offset_rewrite(move || {
            let Some(w) = weak.upgrade() else { return };
            let offset_sec = w.get_lyrics_estimated_offset() as f64;
            let current_path = audio_eng.lock().ok().and_then(|e| e.current_path.clone());
            let Ok(mut s) = state.lock() else { return };
            let idx_opt = s
                .current_filtered_row
                .and_then(|row| s.filtered_tracks.get(row).copied());
            let track_opt = if let Some(ref path) = current_path {
                s.all_tracks.iter_mut().find(|t| &t.audio_path == path)
            } else if let Some(idx) = idx_opt {
                s.all_tracks.get_mut(idx)
            } else {
                None
            };
            let Some(track) = track_opt else { return };

            if let Ok(contents) = fs::read_to_string(&track.lrc_path) {
                let fixed = lyrics_core::rewrite_timestamps_with_offset(&contents, offset_sec);
                if fs::write(&track.lrc_path, &fixed).is_ok() {
                    track.status = LyricsStatus::Synced;
                    track.suspicious_reason = None;
                    if let Ok(db) = open_database() {
                        let _ =
                            db.update_lyrics_status(&track.audio_path, LyricsStatus::Synced, None);
                    }
                    let track_clone = track.clone();
                    drop(s);
                    setup_track_for_playback(
                        &track_clone,
                        &w,
                        &audio_eng,
                        &lyric_st,
                        &cache,
                        &wf_cache,
                        false,
                    );
                    load_library_into_ui(&w, &state, &cache);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        let lyric_st = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        window.on_replace_lyrics(move || {
            let current_path = audio_eng.lock().ok().and_then(|e| e.current_path.clone());
            let Ok(s) = state.lock() else { return };
            let track_opt = if let Some(ref path) = current_path {
                s.all_tracks.iter().find(|t| &t.audio_path == path).cloned()
            } else if let Some(row) = s.current_filtered_row {
                s.filtered_tracks
                    .get(row)
                    .and_then(|&idx| s.all_tracks.get(idx))
                    .cloned()
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
                    let outcome = download_track(
                        &track,
                        DownloadMode::ReplaceAll,
                        &providers,
                        &cancelled,
                        |_| {},
                    )
                    .map_err(|e| e.to_string())?;

                    if let DownloadOutcome::Downloaded { provider, synced } = outcome {
                        db.update_lyrics_status(
                            &track.audio_path,
                            if synced {
                                LyricsStatus::Synced
                            } else {
                                LyricsStatus::Plain
                            },
                            Some(provider.label()),
                        )
                        .map_err(|e| e.to_string())?;
                    }

                    let track_lrc = track.lrc_path.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = event_loop.upgrade() {
                            load_library_into_ui(&w, &state, &art_loader);
                            if let Ok(contents) = fs::read_to_string(&track_lrc) {
                                let lines = parse_lines(&contents);
                                if !lines.is_empty() {
                                    let ui_lines: Vec<LyricLine> = lines
                                        .iter()
                                        .map(|l| LyricLine {
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

    // -----------------------------------------------------------------------
    // Track identification actions (suggest, accept with lofty, or ignore)
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let engine = Arc::clone(&audio);
        let lyrics = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_identify_track(move |row_idx| {
            let Some(w) = weak.upgrade() else { return };
            let Ok(mut s) = state.lock() else { return };
            let row = row_idx as usize;
            let Some(&track_idx) = s.filtered_tracks.get(row) else {
                return;
            };
            s.current_filtered_row = Some(row);
            let Some(track) = s.all_tracks.get(track_idx).cloned() else {
                return;
            };
            drop(s);

            setup_track_for_playback(&track, &w, &engine, &lyrics, &cache, &wf_cache, false);
            w.set_lyrics_visible(true);
            w.set_lyrics_is_identifying(true);
            w.set_lyrics_has_identification_suggestion(false);

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
                    if let Some(w) = event_loop.upgrade() {
                        w.set_lyrics_is_identifying(false);
                        if let Some(sugg) = suggestion {
                            w.set_lyrics_suggested_artist(sugg.suggested_artist.into());
                            w.set_lyrics_suggested_title(sugg.suggested_title.into());
                            w.set_lyrics_suggested_album(
                                sugg.suggested_album.unwrap_or_default().into(),
                            );
                            w.set_lyrics_suggestion_confidence(
                                (sugg.confidence * 100.0).round() as i32
                            );
                            w.set_lyrics_suggestion_reason(sugg.reason.into());
                            w.set_lyrics_has_identification_suggestion(true);
                        } else {
                            w.set_lyrics_has_identification_suggestion(false);
                        }
                    }
                });
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        window.on_identify_current_track(move || {
            let Some(w) = weak.upgrade() else { return };
            let current_path = audio_eng.lock().ok().and_then(|e| e.current_path.clone());
            let Ok(s) = state.lock() else { return };
            let track_opt = if let Some(ref path) = current_path {
                s.all_tracks.iter().find(|t| &t.audio_path == path).cloned()
            } else if let Some(row) = s.current_filtered_row {
                s.filtered_tracks
                    .get(row)
                    .and_then(|&idx| s.all_tracks.get(idx))
                    .cloned()
            } else {
                None
            };
            let Some(track) = track_opt else { return };
            drop(s);

            w.set_lyrics_is_identifying(true);
            w.set_lyrics_has_identification_suggestion(false);

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
                    if let Some(w) = event_loop.upgrade() {
                        w.set_lyrics_is_identifying(false);
                        if let Some(sugg) = suggestion {
                            w.set_lyrics_suggested_artist(sugg.suggested_artist.into());
                            w.set_lyrics_suggested_title(sugg.suggested_title.into());
                            w.set_lyrics_suggested_album(
                                sugg.suggested_album.unwrap_or_default().into(),
                            );
                            w.set_lyrics_suggestion_confidence(
                                (sugg.confidence * 100.0).round() as i32
                            );
                            w.set_lyrics_suggestion_reason(sugg.reason.into());
                            w.set_lyrics_has_identification_suggestion(true);
                        } else {
                            w.set_lyrics_has_identification_suggestion(false);
                        }
                    }
                });
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        let cache = Arc::clone(&art_loader);
        window.on_accept_current_identification(move || {
            let Some(w) = weak.upgrade() else { return };
            let current_path = audio_eng.lock().ok().and_then(|e| e.current_path.clone());
            let Ok(s) = state.lock() else { return };
            let track_opt = if let Some(ref path) = current_path {
                s.all_tracks.iter().find(|t| &t.audio_path == path).cloned()
            } else if let Some(row) = s.current_filtered_row {
                s.filtered_tracks
                    .get(row)
                    .and_then(|&idx| s.all_tracks.get(idx))
                    .cloned()
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
                suggested_album: if suggested_album.trim().is_empty() {
                    None
                } else {
                    Some(suggested_album.clone())
                },
                confidence: 1.0,
                source: "user_accepted".to_string(),
                reason: "Accepted by user".to_string(),
            };

            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let _ = (|| -> Result<(), String> {
                    // 1. Write tags to file using Lofty
                    lyrics_core::apply_tag_suggestion(&track.audio_path, &suggestion)
                        .map_err(|e| format!("Lofty tag write error: {e}"))?;

                    // 2. Update database
                    let db = open_database()?;
                    let album_to_store = suggestion
                        .suggested_album
                        .as_deref()
                        .unwrap_or(&track.album);
                    db.update_track_metadata(
                        &track.audio_path,
                        &suggested_artist,
                        &suggested_title,
                        album_to_store,
                    )
                    .map_err(|e| e.to_string())?;

                    // 3. Update in-memory state
                    track.artist = suggested_artist.clone();
                    track.title = suggested_title.clone();
                    if let Some(ref alb) = suggestion.suggested_album {
                        track.album = alb.clone();
                    }

                    if let Ok(mut s) = state.lock() {
                        if let Some(t) = s
                            .all_tracks
                            .iter_mut()
                            .find(|t| t.audio_path == track.audio_path)
                        {
                            t.artist = track.artist.clone();
                            t.title = track.title.clone();
                            t.album = track.album.clone();
                        }
                    }

                    // 4. Update UI
                    let updated_track = track.clone();
                    let el1 = event_loop.clone();
                    let st1 = Arc::clone(&state);
                    let al1 = Arc::clone(&art_loader);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = el1.upgrade() {
                            w.set_track_title(updated_track.title.clone().into());
                            w.set_track_subtitle(
                                format!("{} • {}", updated_track.artist, updated_track.album)
                                    .into(),
                            );
                            load_library_into_ui(&w, &st1, &al1);
                        }
                    });

                    // 5. Automatically fetch lyrics with newly verified tags
                    let settings = load_settings().map_err(|e| e.to_string())?;
                    let providers = providers_for_settings(&db, &track, &settings)?;
                    let cancelled = AtomicBool::new(false);
                    let outcome = download_track(
                        &track,
                        DownloadMode::ReplaceAll,
                        &providers,
                        &cancelled,
                        |_| {},
                    )
                    .map_err(|e| e.to_string())?;

                    if let DownloadOutcome::Downloaded { provider, synced } = outcome {
                        db.update_lyrics_status(
                            &track.audio_path,
                            if synced {
                                LyricsStatus::Synced
                            } else {
                                LyricsStatus::Plain
                            },
                            Some(provider.label()),
                        )
                        .map_err(|e| e.to_string())?;
                    }

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

    // -----------------------------------------------------------------------
    // Albums and Artists views
    // -----------------------------------------------------------------------
    wire_album_and_artist_views(&window, &app_state, &art_loader);
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let engine = Arc::clone(&audio);
        let lyrics = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_play_album_track(move |row_idx| {
            play_detail_row(
                &weak, &state, &engine, &lyrics, &cache, &wf_cache, row_idx, false,
            );
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let engine = Arc::clone(&audio);
        let lyrics = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_play_artist_track(move |row_idx| {
            play_detail_row(
                &weak, &state, &engine, &lyrics, &cache, &wf_cache, row_idx, true,
            );
        });
    }

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

fn refresh_filtered_health_issues(window: &MainWindow, issues: &[HealthIssue]) {
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

#[cfg(test)]
mod ui_audit;

#[cfg(test)]
mod ui_snapshot;

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn the_waveform_model_holds_exactly_the_bars_that_are_drawn() {
        // The seek bar mutates this model in place, so a length that disagreed
        // with the number of bars the UI repeats would strand stale bars on
        // screen for the rest of the session.
        let wf = default_waveform();
        assert_eq!(wf.len(), WAVEFORM_BARS);
        for &bar in &wf {
            assert!((0.12..=1.0).contains(&bar), "bar out of range: {bar}");
        }
    }

    #[test]
    fn envelope_bars_keeps_the_peak_of_each_window() {
        // Down-sampling takes the loudest sample of each window, not the mean:
        // averaging would flatten a single drum hit into nothing.
        let envelope = [0.1, 0.2, 0.3, 0.9, 0.4, 0.5, 0.6, 0.7];
        assert_eq!(envelope_bars(&envelope, 2), vec![0.9, 0.7]);
        assert_eq!(envelope_bars(&envelope, 8), envelope.to_vec());
        // An empty envelope still has to produce a shape to draw.
        assert_eq!(envelope_bars(&[], WAVEFORM_BARS).len(), WAVEFORM_BARS);
    }

    #[test]
    fn only_a_playing_waveform_rides_the_live_level() {
        let envelope = vec![0.2f32; 100];

        let paused = reactive_bars(&envelope, 0.5, 1.0, false);
        assert!(paused.iter().all(|bar| (*bar - 0.2).abs() < 1e-6));

        let playing = reactive_bars(&envelope, 0.5, 1.0, true);
        let tallest = playing.iter().copied().fold(0.0f32, f32::max);
        assert!(
            tallest > 0.4,
            "the bars at the playhead should be boosted by the live level, got {tallest}"
        );
        // The first bar is far from the playhead so it should not get the
        // local pulse — only the global breath, which at level=1.0 equals
        // the base envelope height.
        assert!((playing[0] - 0.2).abs() < 1e-6);
    }

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
