use crate::art_loader::ArtLoader;
use crate::library_model::{load_library_into_ui, open_database, AppState};
use crate::playback::{setup_track_for_playback, RodioAudioEngine, WaveformCacheMap};
use crate::{LyricLine, LyricWord, MainWindow};
use lyrics_core::{
    find_active_line_index, find_active_word_index, parse_lines, AudioEngine, LrcLine, LrcWord,
    LyricsStatus, Track,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::fs;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Word & Line Synchronization
// ---------------------------------------------------------------------------

pub(crate) fn update_ui_words(
    window: &MainWindow,
    words: &[LrcWord],
    cur_word_idx: i32,
    current_time: f64,
) {
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

pub(crate) fn sync_lyrics_to_position(
    window: &MainWindow,
    lines: &[LrcLine],
    position: f64,
) {
    let offset_ms = window.get_lyrics_offset_ms() as i64;
    let effective_position = (position + (offset_ms as f64 / 1000.0)).max(0.0);

    let active_idx = find_active_line_index(lines, effective_position);
    if window.get_active_line_index() != active_idx {
        window.set_active_line_index(active_idx);
    }

    if active_idx >= 0 && (active_idx as usize) < lines.len() {
        let line = &lines[active_idx as usize];
        if !line.words.is_empty() {
            let cur_word_idx = find_active_word_index(&line.words, effective_position);
            update_ui_words(window, &line.words, cur_word_idx, effective_position);
        } else if window.get_active_has_words() {
            window.set_active_has_words(false);
        }
    } else if window.get_active_has_words() {
        window.set_active_has_words(false);
    }
}

pub(crate) fn setup_lyrics_for_track(
    track: &Track,
    window: &MainWindow,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
) -> bool {
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
    window.set_has_lyrics(has_lyrics);

    if has_lyrics {
        window.set_lyrics_visible(true);
    }

    has_lyrics
}

// ---------------------------------------------------------------------------
// Offset File Saving & Fixing Helpers (<150 lines each)
// ---------------------------------------------------------------------------

fn write_offset_to_file(
    window: &MainWindow,
    state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
) {
    let offset_ms = window.get_lyrics_offset_ms() as i64;
    if offset_ms == 0 {
        return;
    }

    let path_opt = audio.lock().ok().and_then(|a| a.current_path.clone());
    let Some(path) = path_opt else { return };

    let track_opt = state
        .lock()
        .ok()
        .and_then(|s| s.all_tracks.iter().find(|t| t.audio_path == path).cloned());
    let Some(track) = track_opt else { return };

    if let Ok(content) = fs::read_to_string(&track.lrc_path) {
        let offset_sec = offset_ms as f64 / 1000.0;
        let updated = lyrics_core::apply_offset_tag(&content, offset_sec);
        if fs::write(&track.lrc_path, &updated).is_ok() {
            if let Ok(db) = open_database() {
                let _ = db.update_track_offset(&path, 0);
            }
            if let Ok(mut s) = state.lock() {
                if let Some(t) = s.all_tracks.iter_mut().find(|t| t.audio_path == path) {
                    t.lyrics_offset_ms = 0;
                }
            }
            window.set_lyrics_offset_ms(0);
            window.set_lyrics_offset_save_status("Saved to .lrc".into());

            let reloaded = lyrics_core::parse_lines(&updated);
            let ui_lines: Vec<LyricLine> = reloaded
                .iter()
                .map(|l| LyricLine {
                    timestamp: l.timestamp_seconds as f32,
                    text: l.text.clone().into(),
                })
                .collect();
            window.set_lyrics_lines(ModelRc::new(VecModel::from(ui_lines)));
            if let Ok(mut ls) = lyric_state.lock() {
                *ls = reloaded;
            }
        }
    }
}

fn fix_offset_action(
    window: &MainWindow,
    state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &Arc<ArtLoader>,
    waveform_cache: &WaveformCacheMap,
    use_tag: bool,
) {
    let offset_sec = window.get_lyrics_estimated_offset() as f64;
    let current_path = audio.lock().ok().and_then(|e| e.current_path.clone());
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
        let fixed = if use_tag {
            lyrics_core::apply_offset_tag(&contents, offset_sec)
        } else {
            lyrics_core::rewrite_timestamps_with_offset(&contents, offset_sec)
        };
        if fs::write(&track.lrc_path, &fixed).is_ok() {
            track.status = LyricsStatus::Synced;
            track.suspicious_reason = None;
            if let Ok(db) = open_database() {
                let _ = db.update_lyrics_status(&track.audio_path, LyricsStatus::Synced, None);
            }
            let track_clone = track.clone();
            drop(s);
            setup_track_for_playback(
                &track_clone,
                window,
                audio,
                lyric_state,
                art_loader,
                waveform_cache,
                false,
            );
            load_library_into_ui(window, state, art_loader);
        }
    }
}

// ---------------------------------------------------------------------------
// Wiring helper (<150 lines)
// ---------------------------------------------------------------------------

pub(crate) fn wire_lyrics_sync(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    audio: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &Arc<ArtLoader>,
    waveform_cache: &WaveformCacheMap,
) {
    {
        let lyrics = Arc::clone(lyric_state);
        let engine = Arc::clone(audio);
        let weak = window.as_weak();
        window.on_seek_to_line(move |line_idx| {
            let Some(w) = weak.upgrade() else { return };
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
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let audio_eng = Arc::clone(audio);
        window.on_adjust_lyrics_offset(move |delta_ms| {
            let Some(w) = weak.upgrade() else { return };
            let current = w.get_lyrics_offset_ms();
            let new_offset = current + delta_ms;
            w.set_lyrics_offset_ms(new_offset);
            w.set_lyrics_offset_save_status("".into());

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
        let state = Arc::clone(app_state);
        let audio_eng = Arc::clone(audio);
        window.on_set_lyrics_offset(move |target_ms| {
            let Some(w) = weak.upgrade() else { return };
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
        let state = Arc::clone(app_state);
        let audio_eng = Arc::clone(audio);
        let lyric_st = Arc::clone(lyric_state);
        window.on_write_lyrics_offset_to_file(move || {
            if let Some(w) = weak.upgrade() {
                write_offset_to_file(&w, &state, &audio_eng, &lyric_st);
            }
        });
    }
    {
        let lyrics = Arc::clone(lyric_state);
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
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let audio_eng = Arc::clone(audio);
        let lyric_st = Arc::clone(lyric_state);
        let cache = Arc::clone(art_loader);
        let wf_cache = Arc::clone(waveform_cache);
        window.on_fix_offset_tag(move || {
            if let Some(w) = weak.upgrade() {
                fix_offset_action(&w, &state, &audio_eng, &lyric_st, &cache, &wf_cache, true);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let audio_eng = Arc::clone(audio);
        let lyric_st = Arc::clone(lyric_state);
        let cache = Arc::clone(art_loader);
        let wf_cache = Arc::clone(waveform_cache);
        window.on_fix_offset_rewrite(move || {
            if let Some(w) = weak.upgrade() {
                fix_offset_action(&w, &state, &audio_eng, &lyric_st, &cache, &wf_cache, false);
            }
        });
    }
}
