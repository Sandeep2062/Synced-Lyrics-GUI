use crate::matching::{clean_title, score_duration, similarity};
use crate::model::Track;
use crate::providers::lrclib::LrclibRecord;
use lofty::config::WriteOptions;
use lofty::prelude::{Accessor, TagExt, TaggedFileExt};
use lofty::probe::Probe;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// Patterns recognized in filenames during cheap path identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilenamePattern {
    /// "Artist - Title"
    ArtistTitle,
    /// "Title (Artist)" or "Title [Artist]"
    TitleArtist,
    /// "01. Title" or "01 - Title"
    TrackNumTitle,
    /// "01 - Artist - Title" or "01. Artist - Title"
    TrackNumArtistTitle,
    /// "Title" only
    TitleOnly,
}

/// Structured metadata parsed from an audio file's name or path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedFilename {
    pub artist: Option<String>,
    pub title: String,
    pub album: Option<String>,
    pub track_number: Option<u32>,
    pub pattern: FilenamePattern,
}

/// A suggested track identification proposal.
/// Never written to audio tags automatically; must be accepted by the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentificationSuggestion {
    pub current_artist: String,
    pub current_title: String,
    pub current_album: String,
    pub suggested_artist: String,
    pub suggested_title: String,
    pub suggested_album: Option<String>,
    pub confidence: f64, // 0.0 to 1.0
    pub source: String,  // "filename", "lrclib", "audio_fingerprint"
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum TagWriteError {
    #[error("failed to probe audio file: {0}")]
    Probe(String),
    #[error("failed to read audio tags: {0}")]
    Read(String),
    #[error("audio file does not contain a supported tag container")]
    NoTag,
    #[error("failed to save tags to audio file: {0}")]
    Save(String),
}

/// Parse artist, title, and track number from an audio filename or path.
/// Cheap path: executes locally with zero network or audio decoding overhead.
pub fn parse_filename(path_or_filename: &str) -> Option<ParsedFilename> {
    let p = Path::new(path_or_filename);
    let raw_stem = p.file_stem()?.to_str()?.trim();
    if raw_stem.is_empty() {
        return None;
    }

    // Replace underscores with spaces if no spaces exist (e.g. "Cold_Hart_-_2_Am")
    let working_stem = if !raw_stem.contains(' ') && raw_stem.contains('_') {
        raw_stem.replace('_', " ")
    } else {
        raw_stem.to_string()
    };
    let mut stem = working_stem.as_str().trim();

    // 1. Check for leading track number: e.g. "01. ", "01 - ", "01 ", "1. ", "1 - "
    let mut track_number = None;
    if let Some(idx) = stem.find(|c: char| !c.is_ascii_digit()) {
        let digits = &stem[..idx];
        if !digits.is_empty() {
            let tail = &stem[idx..];
            let is_track_prefix = tail.starts_with(". ")
                || tail.starts_with(" - ")
                || tail.starts_with(".) ")
                || tail.starts_with(") ")
                || (digits.len() >= 2 && tail.starts_with(' '));

            if is_track_prefix {
                if let Ok(num) = digits.parse::<u32>() {
                    track_number = Some(num);
                    if let Some(pos) = tail.find(|c: char| c.is_alphanumeric()) {
                        stem = tail[pos..].trim();
                    }
                }
            }
        }
    }

    // 2. Check for "Title (Artist)" or "Title [Artist]"
    if (stem.ends_with(')') && stem.contains('(')) || (stem.ends_with(']') && stem.contains('[')) {
        let (open_char, _close_char) = if stem.ends_with(')') {
            ('(', ')')
        } else {
            ('[', ']')
        };
        if let Some(open_idx) = stem.rfind(open_char) {
            let candidate_artist = stem[open_idx + 1..stem.len() - 1].trim();
            let candidate_title = stem[..open_idx].trim();

            if !candidate_artist.is_empty()
                && !candidate_title.is_empty()
                && !is_bracketed_noise(candidate_artist)
            {
                return Some(ParsedFilename {
                    artist: Some(candidate_artist.to_string()),
                    title: clean_title(candidate_title),
                    album: None,
                    track_number,
                    pattern: FilenamePattern::TitleArtist,
                });
            }
        }
    }

    // 3. Check for "Artist - Title" (or "01 - Artist - Title")
    let sep = if stem.contains(" - ") {
        " - "
    } else if stem.contains(" – ") {
        " – "
    } else if stem.contains(" — ") {
        " — "
    } else {
        ""
    };

    if !sep.is_empty() {
        let parts: Vec<&str> = stem
            .split(sep)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if parts.len() == 2 {
            let artist_part = parts[0];
            let title_part = parts[1];
            return Some(ParsedFilename {
                artist: Some(artist_part.to_string()),
                title: clean_title(title_part),
                album: None,
                track_number,
                pattern: if track_number.is_some() {
                    FilenamePattern::TrackNumArtistTitle
                } else {
                    FilenamePattern::ArtistTitle
                },
            });
        } else if parts.len() >= 3 {
            // Check if parts[0] was track number (e.g. "01 - Artist - Title")
            if parts[0].chars().all(|c| c.is_ascii_digit()) {
                let num = parts[0].parse::<u32>().ok();
                return Some(ParsedFilename {
                    artist: Some(parts[1].to_string()),
                    title: clean_title(parts[2]),
                    album: None,
                    track_number: num.or(track_number),
                    pattern: FilenamePattern::TrackNumArtistTitle,
                });
            } else {
                // E.g. "Artist - Album - Title"
                return Some(ParsedFilename {
                    artist: Some(parts[0].to_string()),
                    album: Some(parts[1].to_string()),
                    title: clean_title(parts[2]),
                    track_number,
                    pattern: FilenamePattern::ArtistTitle,
                });
            }
        }
    }

    // 4. Fallback: track number + title only, or title only
    let pattern = if track_number.is_some() {
        FilenamePattern::TrackNumTitle
    } else {
        FilenamePattern::TitleOnly
    };

    Some(ParsedFilename {
        artist: None,
        title: clean_title(stem),
        album: None,
        track_number,
        pattern,
    })
}

fn is_bracketed_noise(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let noise_prefixes = [
        "feat",
        "ft",
        "prod",
        "produced",
        "official",
        "remaster",
        "live",
        "deluxe",
        "expanded",
        "explicit",
        "clean",
        "version",
        "mix",
        "edit",
        "instrumental",
        "acoustic",
        "mono",
        "stereo",
        "audio",
        "video",
        "visualizer",
        "lyric",
    ];
    let is_year = s.len() == 4
        && s.chars().all(|c| c.is_ascii_digit())
        && (s.starts_with("19") || s.starts_with("20"));

    is_year
        || noise_prefixes
            .iter()
            .any(|&p| lower.starts_with(p) || lower == p)
}

/// Check whether a track needs identification based on its current metadata and file status.
pub fn track_needs_identification(track: &Track) -> bool {
    let artist_empty = track.artist.trim().is_empty()
        || track.artist.eq_ignore_ascii_case("Unknown Artist")
        || track.artist.eq_ignore_ascii_case("Unknown");

    let title_empty = track.title.trim().is_empty()
        || track.title.eq_ignore_ascii_case("Unknown Title")
        || track.title.eq_ignore_ascii_case("Unknown");

    let title_looks_like_file = is_filename_like(&track.title, &track.audio_path);

    artist_empty || title_empty || title_looks_like_file
}

fn is_filename_like(title: &str, audio_path: &str) -> bool {
    let t = title.trim();
    if t.ends_with(".mp3")
        || t.ends_with(".flac")
        || t.ends_with(".m4a")
        || t.ends_with(".wav")
        || t.ends_with(".ogg")
    {
        return true;
    }
    if let Some(stem) = Path::new(audio_path).file_stem().and_then(|s| s.to_str()) {
        if t.eq_ignore_ascii_case(stem) {
            return true;
        }
    }
    false
}

/// Compare parsed filename information against current track tags to produce a suggestion.
pub fn match_filename_against_tags(
    track: &Track,
    parsed: &ParsedFilename,
) -> Option<IdentificationSuggestion> {
    let track_artist = track.artist.trim();
    let track_title = track.title.trim();

    // 1. Detect swapped Artist and Title:
    // e.g. Tags have Artist: "Passion", Title: "Cold Hart", but filename is "Cold Hart - Passion.mp3"
    if let Some(ref fn_artist) = parsed.artist {
        let direct_match =
            similarity(track_artist, fn_artist) + similarity(track_title, &parsed.title);
        let swapped_match =
            similarity(track_artist, &parsed.title) + similarity(track_title, fn_artist);

        if swapped_match >= 1.50 && swapped_match > direct_match + 0.30 {
            return Some(IdentificationSuggestion {
                current_artist: track_artist.to_string(),
                current_title: track_title.to_string(),
                current_album: track.album.clone(),
                suggested_artist: fn_artist.clone(),
                suggested_title: parsed.title.clone(),
                suggested_album: parsed.album.clone().or_else(|| {
                    if !track.album.trim().is_empty() && track.album != "Unknown Album" {
                        Some(track.album.clone())
                    } else {
                        None
                    }
                }),
                confidence: 0.95,
                source: "filename".to_string(),
                reason: format!(
                    "Artist and title appear swapped in tags based on filename pattern '{}'",
                    fn_artist
                ),
            });
        }
    }

    // 2. Missing or filename-like tags when filename has both Artist and Title
    let needs_id = track_needs_identification(track);
    if let Some(ref fn_artist) = parsed.artist {
        if needs_id
            || similarity(track_artist, fn_artist) < 0.60
            || similarity(track_title, &parsed.title) < 0.60
        {
            let confidence = if needs_id { 0.88 } else { 0.78 };
            return Some(IdentificationSuggestion {
                current_artist: track_artist.to_string(),
                current_title: track_title.to_string(),
                current_album: track.album.clone(),
                suggested_artist: fn_artist.clone(),
                suggested_title: parsed.title.clone(),
                suggested_album: parsed.album.clone(),
                confidence,
                source: "filename".to_string(),
                reason: format!(
                    "Parsed artist and title from filename pattern ({:?})",
                    parsed.pattern
                ),
            });
        }
    }

    // 3. Track number in title: e.g. "01. Title" in tag title
    if parsed.pattern == FilenamePattern::TrackNumTitle && track_needs_identification(track) {
        return Some(IdentificationSuggestion {
            current_artist: track_artist.to_string(),
            current_title: track_title.to_string(),
            current_album: track.album.clone(),
            suggested_artist: if track_artist.is_empty() || track_artist == "Unknown Artist" {
                "Unknown Artist".to_string()
            } else {
                track_artist.to_string()
            },
            suggested_title: parsed.title.clone(),
            suggested_album: None,
            confidence: 0.80,
            source: "filename".to_string(),
            reason: "Extracted clean title from filename without track number prefix".to_string(),
        });
    }

    None
}

/// Rank candidates returned from LRCLib title-only search using duration fit + title similarity.
/// Used to guess the artist and fill in missing albums.
pub fn rank_lrclib_candidates(
    track: &Track,
    query_title: &str,
    records: &[LrclibRecord],
) -> Option<IdentificationSuggestion> {
    if records.is_empty() {
        return None;
    }

    let mut best_candidate: Option<(&LrclibRecord, f64, f64, f64)> = None;

    for record in records {
        let rec_title = match record.track_name.as_deref() {
            Some(t) if !t.trim().is_empty() => t.trim(),
            _ => continue,
        };
        let _rec_artist = match record.artist_name.as_deref() {
            Some(a) if !a.trim().is_empty() => a.trim(),
            _ => continue,
        };

        let title_sim = similarity(rec_title, query_title);
        if title_sim < 0.60 {
            continue;
        }

        let (dur_score, dur_known) = score_duration(track.duration_seconds, record.duration);
        if dur_known && dur_score < 0.20 {
            // Significant duration mismatch (> 8.4 seconds)
            continue;
        }

        let rank_score = if dur_known {
            0.55 * dur_score + 0.45 * title_sim
        } else {
            0.70 * title_sim
        };

        if rank_score < 0.50 {
            continue;
        }

        let is_better = match best_candidate {
            Some((_, _, _, best_score)) => rank_score > best_score,
            None => true,
        };

        if is_better {
            best_candidate = Some((record, title_sim, dur_score, rank_score));
        }
    }

    let (record, title_sim, dur_score, rank_score) = best_candidate?;
    let rec_artist = record.artist_name.as_deref().unwrap_or("Unknown Artist");
    let rec_title = record.track_name.as_deref().unwrap_or(query_title);

    let confidence = if track.duration_seconds.is_some() && dur_score >= 0.90 && title_sim >= 0.85 {
        0.92
    } else if track.duration_seconds.is_some() && dur_score >= 0.60 && title_sim >= 0.80 {
        0.82
    } else if title_sim >= 0.85 {
        0.70
    } else {
        (rank_score * 0.9).clamp(0.40, 0.75)
    };

    let missing_album = track.album.trim().is_empty() || track.album == "Unknown Album";
    let suggested_album = if missing_album {
        record.album_name.clone()
    } else {
        Some(track.album.clone())
    };

    Some(IdentificationSuggestion {
        current_artist: track.artist.clone(),
        current_title: track.title.clone(),
        current_album: track.album.clone(),
        suggested_artist: rec_artist.to_string(),
        suggested_title: rec_title.to_string(),
        suggested_album,
        confidence,
        source: "lrclib".to_string(),
        reason: format!(
            "Matched via LRCLib title search (title similarity: {:.0}%, duration fit: {:.0}%)",
            title_sim * 100.0,
            dur_score * 100.0
        ),
    })
}

/// Query LRCLib /api/search by title only and rank candidates.
pub fn identify_via_lrclib_search(
    track: &Track,
    candidate_title: &str,
    base_url: Option<&str>,
) -> Result<Option<IdentificationSuggestion>, String> {
    let base = base_url
        .unwrap_or("https://lrclib.net")
        .trim_end_matches('/');
    let url = format!("{}/api/search", base);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("SyncedLyricsGUI/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    let clean = clean_title(candidate_title);
    let query_param = if clean.is_empty() {
        candidate_title
    } else {
        &clean
    };

    let response = client
        .get(&url)
        .query(&[("track_name", query_param)])
        .send()
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let records: Vec<LrclibRecord> = response.json().map_err(|e| e.to_string())?;
    Ok(rank_lrclib_candidates(track, query_param, &records))
}

/// Unified identification orchestrator:
/// 1. Tries cheap filename parsing first ("Artist - Title", "01. Title", "Title (Artist)").
/// 2. If artist is still missing/unknown or tags look like filenames, searches LRCLib by title only.
/// Returns an optional `IdentificationSuggestion` for user review. Never writes tags automatically.
pub fn identify_track(
    track: &Track,
    lrclib_base_url: Option<&str>,
) -> Option<IdentificationSuggestion> {
    // 1. Cheap path first
    let parsed = parse_filename(&track.audio_path);
    if let Some(ref p) = parsed {
        if let Some(suggestion) = match_filename_against_tags(track, p) {
            // If high confidence filename suggestion (e.g. swapped artist/title or clean pattern match)
            if suggestion.confidence >= 0.85 {
                return Some(suggestion);
            }
        }
    }

    // 2. Title search fallback
    let candidate_title = parsed
        .as_ref()
        .map(|p| p.title.as_str())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(&track.title);

    if !candidate_title.trim().is_empty() {
        if let Ok(Some(lrclib_suggestion)) =
            identify_via_lrclib_search(track, candidate_title, lrclib_base_url)
        {
            return Some(lrclib_suggestion);
        }
    }

    // If LRCLib yielded nothing, fall back to lower-confidence filename suggestion if any
    if let Some(ref p) = parsed {
        return match_filename_against_tags(track, p);
    }

    None
}

/// Safely write accepted tag suggestions to the audio file using Lofty.
/// Preserves embedded pictures/album art, comments, and other audio tags.
pub fn apply_tag_suggestion(
    audio_path: impl AsRef<Path>,
    suggestion: &IdentificationSuggestion,
) -> Result<(), TagWriteError> {
    let path = audio_path.as_ref();
    let mut tagged_file = Probe::open(path)
        .map_err(|e| TagWriteError::Probe(e.to_string()))?
        .read()
        .map_err(|e| TagWriteError::Read(e.to_string()))?;

    let has_tag = tagged_file.primary_tag().is_some() || tagged_file.first_tag().is_some();
    if !has_tag {
        let tag_type = tagged_file.file_type().primary_tag_type();
        tagged_file.insert_tag(lofty::tag::Tag::new(tag_type));
    }

    let tag = if tagged_file.primary_tag().is_some() {
        tagged_file.primary_tag_mut().ok_or(TagWriteError::NoTag)?
    } else {
        tagged_file.first_tag_mut().ok_or(TagWriteError::NoTag)?
    };

    tag.set_artist(suggestion.suggested_artist.clone());
    tag.set_title(suggestion.suggested_title.clone());
    if let Some(ref album) = suggestion.suggested_album {
        if !album.trim().is_empty() {
            tag.set_album(album.clone());
        }
    }

    tag.save_to_path(path, WriteOptions::default())
        .map_err(|e| TagWriteError::Save(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LyricsStatus;

    #[test]
    fn test_filename_patterns() {
        // Pattern 1: "Artist - Title"
        let p1 = parse_filename("Radiohead - Creep.mp3").expect("p1 failed");
        assert_eq!(p1.artist.as_deref(), Some("Radiohead"));
        assert_eq!(p1.title, "Creep");
        assert_eq!(p1.pattern, FilenamePattern::ArtistTitle);

        // Pattern 2: "01. Title" and "01 - Title"
        let p2 = parse_filename("01. Paranoid Android.flac").expect("p2 failed");
        assert_eq!(p2.artist, None);
        assert_eq!(p2.title, "Paranoid Android");
        assert_eq!(p2.track_number, Some(1));
        assert_eq!(p2.pattern, FilenamePattern::TrackNumTitle);

        let p2_hyphen = parse_filename("02 - Karma Police.m4a").expect("p2_hyphen failed");
        assert_eq!(p2_hyphen.artist, None);
        assert_eq!(p2_hyphen.title, "Karma Police");
        assert_eq!(p2_hyphen.track_number, Some(2));
        assert_eq!(p2_hyphen.pattern, FilenamePattern::TrackNumTitle);

        // Pattern 3: "Title (Artist)"
        let p3 = parse_filename("Creep (Radiohead).mp3").expect("p3 failed");
        assert_eq!(p3.artist.as_deref(), Some("Radiohead"));
        assert_eq!(p3.title, "Creep");
        assert_eq!(p3.pattern, FilenamePattern::TitleArtist);

        // Bracketed noise must not be treated as artist: e.g. "Song (Explicit)"
        let p3_noise = parse_filename("2 Am (Explicit).mp3").expect("p3_noise failed");
        assert_eq!(p3_noise.artist, None);
        assert_eq!(p3_noise.title, "2 Am");

        // Pattern 4: "01 - Artist - Title"
        let p4 = parse_filename("03 - Cold Hart - 2 Am.mp3").expect("p4 failed");
        assert_eq!(p4.artist.as_deref(), Some("Cold Hart"));
        assert_eq!(p4.title, "2 Am");
        assert_eq!(p4.track_number, Some(3));
        assert_eq!(p4.pattern, FilenamePattern::TrackNumArtistTitle);

        // Underscore formatted filenames
        let p_under = parse_filename("Cold_Hart_-_2_Am.mp3").expect("p_under failed");
        assert_eq!(p_under.artist.as_deref(), Some("Cold Hart"));
        assert_eq!(p_under.title, "2 Am");
    }

    #[test]
    fn test_swapped_artist_title() {
        // Track where tags have artist and title reversed
        let track = Track {
            audio_path: "D:/Music/Cold Hart - Passion.mp3".to_string(),
            lrc_path: "D:/Music/Cold Hart - Passion.lrc".to_string(),
            artist: "Passion".to_string(), // Swapped: artist contains title!
            title: "Cold Hart".to_string(), // Swapped: title contains artist!
            album: "B4BYANG3L".to_string(),
            duration_seconds: Some(180.0),
            status: LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };

        let parsed = parse_filename(&track.audio_path).expect("parse failed");
        let suggestion = match_filename_against_tags(&track, &parsed).expect("should detect swap");

        assert_eq!(suggestion.suggested_artist, "Cold Hart");
        assert_eq!(suggestion.suggested_title, "Passion");
        assert_eq!(suggestion.suggested_album.as_deref(), Some("B4BYANG3L"));
        assert!(
            suggestion.confidence >= 0.90,
            "Confidence: {}",
            suggestion.confidence
        );
        assert!(suggestion.reason.contains("swapped"));
    }

    #[test]
    fn test_missing_album() {
        // Track with missing album and empty artist/title looking like filename
        let track = Track {
            audio_path: "D:/Music/01. Creep.flac".to_string(),
            lrc_path: "D:/Music/01. Creep.lrc".to_string(),
            artist: "Unknown Artist".to_string(),
            title: "01. Creep.flac".to_string(),
            album: "".to_string(), // Missing album!
            duration_seconds: Some(238.5),
            status: LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };

        let records = vec![
            LrclibRecord {
                track_name: Some("Creep".to_string()),
                artist_name: Some("Radiohead".to_string()),
                album_name: Some("Pablo Honey".to_string()), // Candidate provides album
                duration: Some(238.0),
                synced_lyrics: Some("[00:01]When you were here before".to_string()),
                plain_lyrics: None,
                instrumental: Some(false),
            },
            LrclibRecord {
                track_name: Some("Creep".to_string()),
                artist_name: Some("TLC".to_string()),
                album_name: Some("CrazySexyCool".to_string()),
                duration: Some(268.0), // TLC's Creep is ~4:28 (duration mismatch)
                synced_lyrics: None,
                plain_lyrics: None,
                instrumental: Some(false),
            },
        ];

        let suggestion =
            rank_lrclib_candidates(&track, "Creep", &records).expect("should identify track");

        assert_eq!(suggestion.suggested_artist, "Radiohead");
        assert_eq!(suggestion.suggested_title, "Creep");
        // Must enrich the missing album from candidate!
        assert_eq!(suggestion.suggested_album.as_deref(), Some("Pablo Honey"));
        assert!(
            suggestion.confidence >= 0.85,
            "Confidence: {}",
            suggestion.confidence
        );
    }

    #[test]
    fn test_apply_tag_suggestion_with_lofty() {
        let temp_dir = std::env::temp_dir().join(format!(
            "lofty-id-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("test_song.wav");

        // Generate a minimal valid 44-byte PCM WAV file
        let mut wav_data = Vec::with_capacity(44 + 100);
        wav_data.extend_from_slice(b"RIFF");
        let file_size: u32 = 36 + 100;
        wav_data.extend_from_slice(&file_size.to_le_bytes());
        wav_data.extend_from_slice(b"WAVE");
        wav_data.extend_from_slice(b"fmt ");
        wav_data.extend_from_slice(&16u32.to_le_bytes()); // subchunk1 size (16 for PCM)
        wav_data.extend_from_slice(&1u16.to_le_bytes()); // audio format (1 = PCM)
        wav_data.extend_from_slice(&1u16.to_le_bytes()); // num channels (1 = mono)
        wav_data.extend_from_slice(&44100u32.to_le_bytes()); // sample rate
        wav_data.extend_from_slice(&(44100u32 * 2).to_le_bytes()); // byte rate
        wav_data.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav_data.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav_data.extend_from_slice(b"data");
        wav_data.extend_from_slice(&100u32.to_le_bytes()); // data chunk size
        wav_data.resize(44 + 100, 0);

        std::fs::write(&file_path, &wav_data).unwrap();

        let suggestion = IdentificationSuggestion {
            current_artist: "Unknown Artist".to_string(),
            current_title: "test_song.wav".to_string(),
            current_album: "".to_string(),
            suggested_artist: "Radiohead".to_string(),
            suggested_title: "Creep".to_string(),
            suggested_album: Some("Pablo Honey".to_string()),
            confidence: 0.95,
            source: "filename".to_string(),
            reason: "Parsed from filename".to_string(),
        };

        // Write tags with lofty
        apply_tag_suggestion(&file_path, &suggestion).expect("failed applying tag suggestion");

        // Read back with Lofty Probe
        let tagged = Probe::open(&file_path).unwrap().read().unwrap();
        let tag = tagged
            .primary_tag()
            .or_else(|| tagged.first_tag())
            .expect("should have tag");
        assert_eq!(tag.artist().as_deref(), Some("Radiohead"));
        assert_eq!(tag.title().as_deref(), Some("Creep"));
        assert_eq!(tag.album().as_deref(), Some("Pablo Honey"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
