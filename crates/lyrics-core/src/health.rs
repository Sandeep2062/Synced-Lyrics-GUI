use crate::model::{LyricsStatus, Track};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthCategory {
    Missing,
    WrongSong,
    Offset,
    TooShortOrLong,
    Other,
}

impl HealthCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::WrongSong => "wrong_song",
            Self::Offset => "offset",
            Self::TooShortOrLong => "too_short_long",
            Self::Other => "other",
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Missing => "Missing",
            Self::WrongSong => "Wrong Song",
            Self::Offset => "Offset",
            Self::TooShortOrLong => "Too Short/Long",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthIssue {
    pub audio_path: String,
    pub lrc_path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_seconds: Option<f64>,
    pub status: LyricsStatus,
    pub category: HealthCategory,
    pub reason: String,
    pub is_safe: bool,
    pub suggested_offset_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthStats {
    pub total: usize,
    pub synced: usize,
    pub plain: usize,
    pub missing: usize,
    pub suspicious: usize,
    pub safe_issues: usize,
    pub wrong_song_count: usize,
    pub offset_count: usize,
    pub too_short_long_count: usize,
}

pub fn parse_offset_from_reason(reason: &str) -> Option<i64> {
    // Looks for patterns like "Offset: +1.50s", "Offset: -0.80s", or "Adjust offset (+1.50s)"
    let lower = reason.to_ascii_lowercase();
    if let Some(pos) = lower.find("offset: ") {
        let rest = &reason[pos + 8..];
        let num_str: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '+' || *c == '-')
            .collect();
        if let Ok(sec) = num_str.parse::<f64>() {
            return Some((sec * 1000.0).round() as i64);
        }
    } else if let Some(pos) = lower.find("offset (") {
        let rest = &reason[pos + 8..];
        let num_str: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '+' || *c == '-')
            .collect();
        if let Ok(sec) = num_str.parse::<f64>() {
            return Some((sec * 1000.0).round() as i64);
        }
    }
    None
}

pub fn categorize_track_issue(track: &Track) -> Option<HealthIssue> {
    match track.status {
        LyricsStatus::Synced => None,
        LyricsStatus::Missing => Some(HealthIssue {
            audio_path: track.audio_path.clone(),
            lrc_path: track.lrc_path.clone(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_seconds: track.duration_seconds,
            status: LyricsStatus::Missing,
            category: HealthCategory::Missing,
            reason: "Missing lyrics file".to_string(),
            is_safe: true,
            suggested_offset_ms: None,
        }),
        LyricsStatus::Plain => Some(HealthIssue {
            audio_path: track.audio_path.clone(),
            lrc_path: track.lrc_path.clone(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_seconds: track.duration_seconds,
            status: LyricsStatus::Plain,
            category: HealthCategory::Other,
            reason: "Plain unsynchronized lyrics".to_string(),
            is_safe: false,
            suggested_offset_ms: None,
        }),
        LyricsStatus::Suspicious => {
            let reason_raw = track
                .suspicious_reason
                .as_deref()
                .unwrap_or("Suspicious lyrics");
            let reason_lower = reason_raw.to_ascii_lowercase();

            let (category, reason_display, offset_ms) = if reason_lower.contains("wrong song")
                || reason_lower.contains("title metadata mismatch")
                || reason_lower.contains("titlemismatch")
            {
                (
                    HealthCategory::WrongSong,
                    "Wrong song detected".to_string(),
                    None,
                )
            } else if reason_lower.contains("offset") {
                let offset_ms = parse_offset_from_reason(reason_raw);
                let label = if let Some(ms) = offset_ms {
                    format!("Offset ({:+.2}s)", ms as f64 / 1000.0)
                } else {
                    "Timing offset detected".to_string()
                };
                (HealthCategory::Offset, label, offset_ms)
            } else if reason_lower.contains("exceed") || reason_lower.contains("too long") {
                (
                    HealthCategory::TooShortOrLong,
                    "Lyrics exceed track duration (too long)".to_string(),
                    None,
                )
            } else if reason_lower.contains("end too early")
                || reason_lower.contains("too few")
                || reason_lower.contains("too short")
            {
                (
                    HealthCategory::TooShortOrLong,
                    "Lyrics end too early (too short)".to_string(),
                    None,
                )
            } else {
                (HealthCategory::Other, reason_raw.to_string(), None)
            };

            Some(HealthIssue {
                audio_path: track.audio_path.clone(),
                lrc_path: track.lrc_path.clone(),
                title: track.title.clone(),
                artist: track.artist.clone(),
                album: track.album.clone(),
                duration_seconds: track.duration_seconds,
                status: LyricsStatus::Suspicious,
                category,
                reason: reason_display,
                is_safe: true,
                suggested_offset_ms: offset_ms,
            })
        }
    }
}

pub fn compute_health_summary(tracks: &[Track]) -> (HealthStats, Vec<HealthIssue>) {
    let mut stats = HealthStats {
        total: tracks.len(),
        ..HealthStats::default()
    };
    let mut issues = Vec::new();

    for track in tracks {
        match track.status {
            LyricsStatus::Synced => stats.synced += 1,
            LyricsStatus::Plain => stats.plain += 1,
            LyricsStatus::Missing => stats.missing += 1,
            LyricsStatus::Suspicious => stats.suspicious += 1,
        }

        if let Some(issue) = categorize_track_issue(track) {
            match issue.category {
                HealthCategory::WrongSong => stats.wrong_song_count += 1,
                HealthCategory::Offset => stats.offset_count += 1,
                HealthCategory::TooShortOrLong => stats.too_short_long_count += 1,
                _ => {}
            }
            if issue.is_safe {
                stats.safe_issues += 1;
            }
            issues.push(issue);
        }
    }

    (stats, issues)
}

/// Applies an offset adjustment (in ms) to LRC content by updating/inserting the `[offset:]` tag.
pub fn apply_offset_to_lrc(lrc_text: &str, offset_ms: i64) -> String {
    let mut lines: Vec<String> = lrc_text.lines().map(|s| s.to_string()).collect();
    let mut found = false;

    for line in lines.iter_mut() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("[offset:") {
            if let Some(close) = rest.find(']') {
                let inner = &rest[..close];
                let existing_val: i64 = inner.trim().parse().unwrap_or(0);
                let new_val = existing_val + offset_ms;
                let remainder = &rest[close + 1..];
                *line = format!("[offset: {}]{}", new_val, remainder);
                found = true;
                break;
            }
        }
    }

    if !found {
        lines.insert(0, format!("[offset: {}]", offset_ms));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_offset_from_reason() {
        assert_eq!(parse_offset_from_reason("Offset: +1.50s"), Some(1500));
        assert_eq!(parse_offset_from_reason("Offset: -0.80s"), Some(-800));
        assert_eq!(parse_offset_from_reason("Adjust offset (+0.50s)"), Some(500));
        assert_eq!(
            parse_offset_from_reason("Words or lines out of order"),
            None
        );
    }

    #[test]
    fn test_categorize_track_issues() {
        let missing = Track {
            audio_path: "/a/missing.mp3".into(),
            lrc_path: "/a/missing.lrc".into(),
            artist: "Artist".into(),
            title: "Missing Song".into(),
            album: "Album".into(),
            duration_seconds: Some(180.0),
            status: LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };
        let issue = categorize_track_issue(&missing).unwrap();
        assert_eq!(issue.category, HealthCategory::Missing);
        assert!(issue.is_safe);

        let wrong_song = Track {
            status: LyricsStatus::Suspicious,
            suspicious_reason: Some("Wrong song detected".into()),
            ..missing.clone()
        };
        let issue = categorize_track_issue(&wrong_song).unwrap();
        assert_eq!(issue.category, HealthCategory::WrongSong);
        assert!(issue.is_safe);

        let offset_track = Track {
            status: LyricsStatus::Suspicious,
            suspicious_reason: Some("Offset: +1.20s".into()),
            ..missing.clone()
        };
        let issue = categorize_track_issue(&offset_track).unwrap();
        assert_eq!(issue.category, HealthCategory::Offset);
        assert_eq!(issue.suggested_offset_ms, Some(1200));

        let too_short = Track {
            status: LyricsStatus::Suspicious,
            suspicious_reason: Some("Lyrics end too early".into()),
            ..missing.clone()
        };
        let issue = categorize_track_issue(&too_short).unwrap();
        assert_eq!(issue.category, HealthCategory::TooShortOrLong);
    }

    #[test]
    fn test_apply_offset_to_lrc() {
        let initial = "[ar: Artist]\n[00:10.00]Hello world";
        let adjusted = apply_offset_to_lrc(initial, 500);
        assert!(adjusted.starts_with("[offset: 500]\n"));

        let existing_offset = "[offset: 200]\n[00:10.00]Hello world";
        let adjusted2 = apply_offset_to_lrc(existing_offset, 300);
        assert!(adjusted2.starts_with("[offset: 500]"));
    }

    #[test]
    fn test_apply_offset_to_lrc_malformed_and_whitespace() {
        // Leading spaces
        let lrc = "    [offset: 200]\n[00:01.00] test";
        let res = apply_offset_to_lrc(lrc, 100);
        assert!(res.contains("[offset: 300]"));

        // Leading unicode fullwidth space
        let lrc_unicode = "　　[offset: -50]\n[00:01.00] test";
        let res2 = apply_offset_to_lrc(lrc_unicode, 100);
        assert!(res2.contains("[offset: 50]"));

        // Malformed offset with no number
        let lrc_empty_num = "[offset: ]\n[00:01.00] test";
        let res3 = apply_offset_to_lrc(lrc_empty_num, 250);
        assert!(res3.contains("[offset: 250]"));

        // Malformed without closing bracket
        let lrc_unclosed = "[offset: 100";
        let res4 = apply_offset_to_lrc(lrc_unclosed, 50);
        assert!(res4.starts_with("[offset: 50]"));
    }
}
