use crate::art_loader::{album_cover_image, track_cover_now, ArtLoader};
use crate::audio_source;
use crate::library_model::{format_time, AppState};
use crate::lyrics_sync::*;
use crate::MainWindow;
use lofty::prelude::AudioFile;
use lyrics_core::{
    find_active_line_index, find_active_word_index, AudioEngine, LrcLine, MutexExt, Track,
    WaveformStore,
};
use rodio::Source;
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Audio engine
// ---------------------------------------------------------------------------

pub(crate) struct RodioAudioEngine {
    pub(crate) _stream: Option<rodio::OutputStream>,
    pub(crate) stream_handle: Option<rodio::OutputStreamHandle>,
    pub(crate) sink: Option<rodio::Sink>,
    pub(crate) current_path: Option<String>,
    pub(crate) duration: Option<f64>,
    pub(crate) volume: f32,
    pub(crate) speed: f32,
    /// Live RMS amplitude from the audio stream, updated ~every 2048 samples.
    pub(crate) live_level: audio_source::LiveLevel,
}

impl RodioAudioEngine {
    pub(crate) fn new() -> Self {
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

/// Decoded per-track envelopes, keyed by audio path. Shared through an `Arc` so
/// the playback timer can read one 30 times a second without copying it.
pub(crate) type WaveformCacheMap = Arc<Mutex<HashMap<String, Arc<Vec<f32>>>>>;

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
pub(crate) fn synthetic_shape(count: usize) -> Vec<f32> {
    vec![0.15f32; count]
}

pub(crate) fn default_waveform() -> Vec<f32> {
    synthetic_shape(WAVEFORM_BARS)
}

/// Peak of each display bar's window of `envelope`. Taking the peak rather than
/// the mean keeps short, loud hits visible after down-sampling.
pub(crate) fn envelope_bars(envelope: &[f32], bars: usize) -> Vec<f32> {
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
/// changed.
pub(crate) fn write_waveform_bars(model: &VecModel<f32>, values: &[f32]) {
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

/// The bars to draw for `progress` (0..1) through the track.
pub(crate) fn reactive_bars(
    envelope: &[f32],
    progress: f32,
    level: f32,
    playing: bool,
) -> Vec<f32> {
    let mut bars = envelope_bars(envelope, WAVEFORM_BARS);
    if !playing {
        return bars;
    }
    let intensity = level * level;

    for (index, bar) in bars.iter_mut().enumerate() {
        let fraction = (index as f32 + 0.5) / WAVEFORM_BARS as f32;
        let base = *bar;

        let floor = 0.08;
        let height = floor + (base - floor).max(0.0) * intensity;

        let dist = (progress - fraction).abs();
        let near = (1.0 - dist / PULSE_WIDTH).clamp(0.0, 1.0);
        let pulse = near * near * level * 0.5;

        *bar = (height + pulse).clamp(0.06, 1.0);
    }
    bars
}

/// Pushes `values` into the seek bar's waveform model in place.
pub(crate) fn set_waveform_bars(window: &MainWindow, values: &[f32]) {
    let model = window.get_waveform_data();
    match model.as_any().downcast_ref::<VecModel<f32>>() {
        Some(vec) => write_waveform_bars(vec, values),
        None => window.set_waveform_data(ModelRc::new(VecModel::from(values.to_vec()))),
    }
}

pub(crate) fn compute_audio_waveform(audio_path: &str) -> Vec<f32> {
    audio_source::waveform_bars(std::path::Path::new(audio_path), ENVELOPE_BUCKETS, || {
        synthetic_shape(ENVELOPE_BUCKETS)
    })
}

// ---------------------------------------------------------------------------
// Track setup & waveform loading
// ---------------------------------------------------------------------------

pub(crate) fn setup_track_for_playback(
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

    let cover_opt =
        track_cover_now(track, art_loader).or_else(|| album_cover_image(&track.album, art_loader));
    if let Some(ref cover) = cover_opt {
        window.set_track_cover(cover.clone());
        window.set_has_cover(true);
        art_loader.set_player_album(None);
    } else {
        window.set_track_cover(slint::Image::default());
        window.set_has_cover(false);
        art_loader.set_player_album(Some(track.album.clone()));
    }

    let stamp = track.mtime.unwrap_or(0.0) as u64;
    let path = track.audio_path.clone();

    let cached_envelope = {
        let cache = waveform_cache.lock_ignore_poison();
        cache.get(&path).cloned()
    };

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

    setup_lyrics_for_track(track, window, lyric_state);

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

pub(crate) fn prefetch_track_waveform(track: &Track, waveform_cache: &WaveformCacheMap) {
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

pub(crate) fn prefetch_next_track(
    app_state: &Arc<Mutex<AppState>>,
    waveform_cache: &WaveformCacheMap,
) {
    let next_track = {
        let Ok(s) = app_state.lock() else { return };
        if s.filtered_tracks.is_empty() {
            return;
        }
        let next_row = match s.current_filtered_row {
            Some(cur) => (cur + 1) % s.filtered_tracks.len(),
            None => 0,
        };
        s.filtered_tracks
            .get(next_row)
            .and_then(|&idx| s.all_tracks.get(idx))
            .cloned()
    };
    if let Some(track) = next_track {
        prefetch_track_waveform(&track, waveform_cache);
    }
}

pub(crate) fn play_next_track(
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
        s.filtered_tracks
            .get(next_row)
            .and_then(|&idx| s.all_tracks.get(idx))
            .cloned()
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

pub(crate) fn play_prev_track(
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
        s.filtered_tracks
            .get(prev_row)
            .and_then(|&idx| s.all_tracks.get(idx))
            .cloned()
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn play_detail_row(
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
// Wiring helpers (<150 lines each)
// ---------------------------------------------------------------------------

pub(crate) fn wire_track_triggers(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &Arc<ArtLoader>,
    waveform_cache: &WaveformCacheMap,
) {
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let lyrics = Arc::clone(lyric_state);
        let engine = Arc::clone(audio);
        let cache = Arc::clone(art_loader);
        let wf_cache = Arc::clone(waveform_cache);
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
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let engine = Arc::clone(audio);
        let lyrics = Arc::clone(lyric_state);
        let cache = Arc::clone(art_loader);
        let wf_cache = Arc::clone(waveform_cache);
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
        let state = Arc::clone(app_state);
        let engine = Arc::clone(audio);
        let lyrics = Arc::clone(lyric_state);
        let cache = Arc::clone(art_loader);
        let wf_cache = Arc::clone(waveform_cache);
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
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let engine = Arc::clone(audio);
        let lyrics = Arc::clone(lyric_state);
        let cache = Arc::clone(art_loader);
        let wf_cache = Arc::clone(waveform_cache);
        window.on_play_album_track(move |row_idx| {
            play_detail_row(
                &weak, &state, &engine, &lyrics, &cache, &wf_cache, row_idx, false,
            );
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let engine = Arc::clone(audio);
        let lyrics = Arc::clone(lyric_state);
        let cache = Arc::clone(art_loader);
        let wf_cache = Arc::clone(waveform_cache);
        window.on_play_artist_track(move |row_idx| {
            play_detail_row(
                &weak, &state, &engine, &lyrics, &cache, &wf_cache, row_idx, true,
            );
        });
    }
}

pub(crate) fn wire_transport_controls(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &Arc<ArtLoader>,
    waveform_cache: &WaveformCacheMap,
) {
    {
        let engine = Arc::clone(audio);
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
        let engine = Arc::clone(audio);
        let lyrics = Arc::clone(lyric_state);
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
                w.set_seek_position(fraction);
                w.set_position_text(format_time(target_pos).into());
                let offset_ms = w.get_lyrics_offset_ms() as i64;
                let effective_pos = (target_pos + (offset_ms as f64 / 1000.0)).max(0.0);
                if let Ok(lines) = lyrics.lock() {
                    let active_idx = find_active_line_index(&lines, effective_pos);
                    w.set_active_line_index(active_idx);
                    if active_idx >= 0 {
                        if let Some(line) = lines.get(active_idx as usize) {
                            if !line.words.is_empty() {
                                let cur_word = find_active_word_index(&line.words, effective_pos);
                                update_ui_words(&w, &line.words, cur_word, effective_pos);
                            } else if w.get_active_has_words() {
                                w.set_active_has_words(false);
                            }
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
        let engine = Arc::clone(audio);
        window.on_rewind(move || {
            if let Ok(mut eng) = engine.lock() {
                if let Some(pos) = eng.position_seconds() {
                    let _ = eng.seek((pos - 10.0).max(0.0));
                }
            }
        });
    }
    {
        let engine = Arc::clone(audio);
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
        let state = Arc::clone(app_state);
        let audio_eng = Arc::clone(audio);
        let lyr = Arc::clone(lyric_state);
        let art = Arc::clone(art_loader);
        let wf = Arc::clone(waveform_cache);
        window.on_next_track(move || {
            if let Some(w) = weak.upgrade() {
                play_next_track(&w, &state, &audio_eng, &lyr, &art, &wf);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let audio_eng = Arc::clone(audio);
        let lyr = Arc::clone(lyric_state);
        let art = Arc::clone(art_loader);
        let wf = Arc::clone(waveform_cache);
        window.on_prev_track(move || {
            if let Some(w) = weak.upgrade() {
                play_prev_track(&w, &state, &audio_eng, &lyr, &art, &wf);
            }
        });
    }
    {
        let engine = Arc::clone(audio);
        let weak = window.as_weak();
        window.on_volume_changed(move |vol| {
            let Some(w) = weak.upgrade() else { return };
            let vol_clamped = vol.clamp(0.0, 1.0);
            w.set_volume(vol_clamped);
            if vol_clamped > 0.001 && w.get_is_muted() {
                w.set_is_muted(false);
            }
            if let Ok(mut eng) = engine.lock() {
                if !w.get_is_muted() {
                    eng.set_volume(vol_clamped);
                }
            }
            if let Ok(mut settings) = lyrics_core::load_settings() {
                settings.volume = vol_clamped;
                settings.muted = w.get_is_muted();
                let _ = lyrics_core::save_settings(&settings);
            }
        });
    }
    {
        let engine = Arc::clone(audio);
        let weak = window.as_weak();
        window.on_toggle_mute(move || {
            let Some(w) = weak.upgrade() else { return };
            let currently_muted = w.get_is_muted();
            let new_muted = !currently_muted;
            w.set_is_muted(new_muted);

            let vol = w.get_volume();
            let effective_vol = if new_muted {
                0.0
            } else if vol <= 0.001 {
                w.set_volume(0.5);
                0.5
            } else {
                vol
            };

            if let Ok(mut eng) = engine.lock() {
                eng.set_volume(effective_vol);
            }

            if let Ok(mut settings) = lyrics_core::load_settings() {
                settings.muted = new_muted;
                settings.volume = w.get_volume();
                let _ = lyrics_core::save_settings(&settings);
            }
        });
    }
}

pub(crate) fn start_playback_timer(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &Arc<ArtLoader>,
    waveform_cache: &WaveformCacheMap,
) -> Timer {
    let timer_audio = Arc::clone(audio);
    let timer_lyrics = Arc::clone(lyric_state);
    let timer_app_state = Arc::clone(app_state);
    let timer_art_loader = Arc::clone(art_loader);
    let timer_wf_cache = Arc::clone(waveform_cache);
    let timer_window = window.as_weak();
    let mut level = 0.0f32;
    let mut level_path: Option<String> = None;

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

        let Some(position) = position_opt else { return };

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
        let rate = if target > level { 0.7 } else { 0.2 };
        level += (target - level) * rate;
        set_waveform_bars(&window, &reactive_bars(env_slice, progress, level, playing));

        if let Ok(lines) = timer_lyrics.lock() {
            sync_lyrics_to_position(&window, &lines, position);
        }

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

    position_timer
}

pub(crate) fn wire_playback(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &Arc<ArtLoader>,
    waveform_cache: &WaveformCacheMap,
) -> Timer {
    wire_track_triggers(
        window,
        app_state,
        audio,
        lyric_state,
        art_loader,
        waveform_cache,
    );
    wire_transport_controls(
        window,
        app_state,
        audio,
        lyric_state,
        art_loader,
        waveform_cache,
    );
    start_playback_timer(
        window,
        app_state,
        audio,
        lyric_state,
        art_loader,
        waveform_cache,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn the_waveform_model_holds_exactly_the_bars_that_are_drawn() {
        let wf = default_waveform();
        assert_eq!(wf.len(), WAVEFORM_BARS);
        for &bar in &wf {
            assert!((0.12..=1.0).contains(&bar), "bar out of range: {bar}");
        }
    }

    #[test]
    fn envelope_bars_keeps_the_peak_of_each_window() {
        let envelope = [0.1, 0.2, 0.3, 0.9, 0.4, 0.5, 0.6, 0.7];
        assert_eq!(envelope_bars(&envelope, 2), vec![0.9, 0.7]);
        assert_eq!(envelope_bars(&envelope, 8), envelope.to_vec());
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
        assert!((playing[0] - 0.2).abs() < 1e-6);
    }
}
