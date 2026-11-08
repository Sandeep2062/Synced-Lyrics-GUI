use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq)]
pub struct LrcLine {
    pub timestamp_seconds: f64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LrcIssue {
    Empty,
    NotSynced,
    TooFewTimestamps,
    TooLong,
    EndsTooEarly,
    TitleMismatch,
}

/// Metadata extracted from standard LRC ID tags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LrcMetadata {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    /// Overall timestamp adjustment in milliseconds.
    /// Per the LRC specification convention used here:
    /// Positive offset shifts lyrics earlier (i.e. timestamp_seconds = raw - offset_ms / 1000).
    /// Negative offset shifts lyrics later (i.e. timestamp_seconds = raw + |offset_ms| / 1000).
    pub offset: Option<i64>,
    pub length: Option<String>,
}

impl LrcMetadata {
    /// Return the offset in seconds (positive = lyrics appear earlier).
    pub fn offset_seconds(&self) -> f64 {
        self.offset.unwrap_or(0) as f64 / 1000.0
    }
}

const PAST_END_TOLERANCE: f64 = 5.0;
const EARLY_END_RATIO: f64 = 0.55;
const EARLY_END_GAP: f64 = 15.0;

/// Parse metadata tags (`[ar:]`, `[ti:]`, `[al:]`, `[offset:]`, `[length:]`) from LRC text.
pub fn parse_metadata(text: &str) -> LrcMetadata {
    let mut metadata = LrcMetadata::default();
    for line in text.lines() {
        let mut remaining = line.trim();
        while let Some(start) = remaining.find('[') {
            let Some(close_offset) = remaining[start..].find(']') else {
                break;
            };
            let close = start + close_offset;
            let tag = &remaining[start + 1..close];
            if let Some((key, val)) = tag.split_once(':') {
                let key = key.trim().to_ascii_lowercase();
                let val = val.trim();
                match key.as_str() {
                    "ar" if metadata.artist.is_none() && !val.is_empty() => {
                        metadata.artist = Some(val.to_string());
                    }
                    "ti" if metadata.title.is_none() && !val.is_empty() => {
                        metadata.title = Some(val.to_string());
                    }
                    "al" if metadata.album.is_none() && !val.is_empty() => {
                        metadata.album = Some(val.to_string());
                    }
                    "offset" if metadata.offset.is_none() => {
                        let clean = val.replace(' ', "");
                        if let Ok(offset_ms) = clean.parse::<i64>() {
                            metadata.offset = Some(offset_ms);
                        }
                    }
                    "length" if metadata.length.is_none() && !val.is_empty() => {
                        metadata.length = Some(val.to_string());
                    }
                    _ => {}
                }
            }
            remaining = &remaining[close + 1..];
        }
    }
    metadata
}

/// Parse all timestamps from LRC text into a sorted list of seconds.
/// Applies `[offset:]` metadata if present (positive = lyrics appear earlier, clamped to 0.0).
pub fn parse_timestamps(text: &str) -> Vec<f64> {
    let metadata = parse_metadata(text);
    let offset_seconds = metadata.offset_seconds();
    let mut timestamps = Vec::new();
    for line in text.lines() {
        let mut remaining = line;
        while let Some(start) = remaining.find('[') {
            let Some(end_offset) = remaining[start..].find(']') else {
                break;
            };
            let end = start + end_offset;
            let tag = &remaining[start + 1..end];
            if let Some(seconds) = parse_timestamp(tag) {
                let adjusted = (seconds - offset_seconds).max(0.0);
                timestamps.push(adjusted);
            }
            remaining = &remaining[end + 1..];
        }
    }
    timestamps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    timestamps
}

/// Parse lyrics lines and timestamps from LRC text into sorted `LrcLine` items.
///
/// Features:
/// - Panic-free on any input (empty strings, leading whitespace, multi-byte characters, unmatched brackets).
/// - Supports multiple timestamps per line (e.g. `[00:10][00:50]text` produces two `LrcLine` entries).
/// - Applies `[offset:]` metadata (positive = lyrics appear earlier, clamped to 0.0).
pub fn parse_lines(text: &str) -> Vec<LrcLine> {
    let metadata = parse_metadata(text);
    let offset_seconds = metadata.offset_seconds();
    let mut lines = Vec::new();

    for raw_line in text.lines() {
        let mut remaining = raw_line.trim_start();
        let mut line_timestamps = Vec::new();

        while remaining.starts_with('[') {
            let Some(close) = remaining.find(']') else {
                break;
            };
            let tag = &remaining[1..close];
            if let Some(ts) = parse_timestamp(tag) {
                line_timestamps.push(ts);
                remaining = remaining[close + 1..].trim_start();
            } else {
                break;
            }
        }

        if line_timestamps.is_empty() {
            continue;
        }

        let lyric_text = remaining.trim().to_string();
        for ts in line_timestamps {
            let adjusted = (ts - offset_seconds).max(0.0);
            lines.push(LrcLine {
                timestamp_seconds: adjusted,
                text: lyric_text.clone(),
            });
        }
    }

    lines.sort_by(|a, b| {
        a.timestamp_seconds
            .partial_cmp(&b.timestamp_seconds)
            .unwrap_or(Ordering::Equal)
    });
    lines
}

pub fn is_synced(text: &str) -> bool {
    parse_timestamps(text).len() >= 3
}

pub use crate::matching::{normalize_title, similarity, titles_match};

pub fn audit_lrc(text: &str, duration_seconds: Option<f64>, title: &str) -> Option<LrcIssue> {
    if text.trim().is_empty() {
        return Some(LrcIssue::Empty);
    }
    let timestamps = parse_timestamps(text);
    if timestamps.is_empty() {
        return Some(LrcIssue::NotSynced);
    }
    if timestamps.len() < 3 {
        return Some(LrcIssue::TooFewTimestamps);
    }
    if let Some(duration) = duration_seconds.filter(|duration| *duration > 0.0) {
        let last = *timestamps.last().unwrap();
        if last > duration + PAST_END_TOLERANCE {
            return Some(LrcIssue::TooLong);
        }
        if last < duration * EARLY_END_RATIO && duration - last > EARLY_END_GAP {
            return Some(LrcIssue::EndsTooEarly);
        }
    }
    if let Some(ref tag_title) = metadata_title(text) {
        if !title.is_empty() && !titles_match(title, tag_title) {
            return Some(LrcIssue::TitleMismatch);
        }
    }
    None
}

/// Accept timestamps in formats:
/// - `[mm:ss]` (e.g. `01:02`)
/// - `[mm:ss.xx]` (e.g. `01:02.34`)
/// - `[mm:ss.xxx]` (e.g. `01:02.345`)
/// - `[mm:ss:xx]` (e.g. `01:02:34` or `01:02:345`)
fn parse_timestamp(value: &str) -> Option<f64> {
    let value = value.trim();
    let parts: Vec<&str> = value.split(':').collect();
    match parts.as_slice() {
        [minutes_str, seconds_str] => {
            let minutes = minutes_str.trim().parse::<u64>().ok()?;
            let seconds_str = seconds_str.trim();
            if !seconds_str.starts_with(|c: char| c.is_ascii_digit()) {
                return None;
            }
            let seconds = seconds_str.parse::<f64>().ok()?;
            if !seconds.is_finite() || seconds >= 60.0 || seconds < 0.0 {
                return None;
            }
            Some(minutes as f64 * 60.0 + seconds)
        }
        [minutes_str, seconds_str, fraction_str] => {
            let minutes = minutes_str.trim().parse::<u64>().ok()?;
            let seconds_str = seconds_str.trim();
            if !seconds_str.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let seconds = seconds_str.parse::<u64>().ok()?;
            if seconds >= 60 {
                return None;
            }
            let fraction_str = fraction_str.trim();
            if fraction_str.is_empty() || !fraction_str.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let fraction = fraction_str.parse::<f64>().ok()?;
            let frac_sec = fraction / 10f64.powi(fraction_str.len() as i32);
            let total = minutes as f64 * 60.0 + seconds as f64 + frac_sec;
            if !total.is_finite() {
                return None;
            }
            Some(total)
        }
        _ => None,
    }
}

fn metadata_title(text: &str) -> Option<String> {
    parse_metadata(text).title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_and_three_digit_centiseconds() {
        assert_eq!(
            parse_timestamps("[01:02.34]a\n[01:03.456]b"),
            vec![62.34, 63.456]
        );
    }

    #[test]
    fn parse_lines_does_not_panic_on_bracket_or_unicode() {
        let lines1 = parse_lines("]");
        assert!(lines1.is_empty());

        let lines2 = parse_lines("🦀]foo");
        assert!(lines2.is_empty());

        let lines3 = parse_lines("");
        assert!(lines3.is_empty());

        let lines4 = parse_lines("   \n\t  \r\n");
        assert!(lines4.is_empty());

        let lines5 = parse_lines("[");
        assert!(lines5.is_empty());

        let lines6 = parse_lines("[]");
        assert!(lines6.is_empty());

        let lines7 = parse_lines("[-01:00]negative min");
        assert!(lines7.is_empty());

        let lines8 = parse_lines("[01:60]seconds out of bounds");
        assert!(lines8.is_empty());

        let lines9 = parse_lines("[01:02]🦀 Unicode text 🎶");
        assert_eq!(lines9.len(), 1);
        assert_eq!(lines9[0].text, "🦀 Unicode text 🎶");
        assert_eq!(lines9[0].timestamp_seconds, 62.0);
    }

    #[test]
    fn parse_lines_handles_leading_whitespace() {
        let lines = parse_lines("  [01:00]hello");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[0].timestamp_seconds, 60.0);
    }

    #[test]
    fn parse_lines_supports_multiple_timestamps() {
        let lines = parse_lines("[00:10][00:50]text");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].timestamp_seconds, 10.0);
        assert_eq!(lines[0].text, "text");
        assert_eq!(lines[1].timestamp_seconds, 50.0);
        assert_eq!(lines[1].text, "text");

        let lines_with_spaces = parse_lines("   [00:10]   [00:50]   text with spaces  ");
        assert_eq!(lines_with_spaces.len(), 2);
        assert_eq!(lines_with_spaces[0].timestamp_seconds, 10.0);
        assert_eq!(lines_with_spaces[0].text, "text with spaces");
        assert_eq!(lines_with_spaces[1].timestamp_seconds, 50.0);
        assert_eq!(lines_with_spaces[1].text, "text with spaces");
    }

    #[test]
    fn parse_timestamps_supports_multiple_timestamps_per_line() {
        assert_eq!(
            parse_timestamps("[00:10][00:50]text\n[01:00]more"),
            vec![10.0, 50.0, 60.0]
        );
    }

    #[test]
    fn accepts_all_required_timestamp_formats() {
        // [mm:ss]
        assert_eq!(parse_timestamps("[01:02]text"), vec![62.0]);
        // [mm:ss.xx]
        assert_eq!(parse_timestamps("[01:02.34]text"), vec![62.34]);
        // [mm:ss.xxx]
        assert_eq!(parse_timestamps("[01:02.345]text"), vec![62.345]);
        // [mm:ss:xx]
        assert_eq!(parse_timestamps("[01:02:34]text"), vec![62.34]);
        // [mm:ss:xxx]
        assert_eq!(parse_timestamps("[01:02:345]text"), vec![62.345]);
    }

    #[test]
    fn parses_metadata_tags() {
        let lrc = "\
[ar: The Beatles]
[ti: Hey Jude]
[al: Past Masters]
[offset: +500]
[length: 07:11]
[00:01.00]Hey Jude";

        let meta = parse_metadata(lrc);
        assert_eq!(meta.artist.as_deref(), Some("The Beatles"));
        assert_eq!(meta.title.as_deref(), Some("Hey Jude"));
        assert_eq!(meta.album.as_deref(), Some("Past Masters"));
        assert_eq!(meta.offset, Some(500));
        assert_eq!(meta.length.as_deref(), Some("07:11"));
        assert_eq!(meta.offset_seconds(), 0.5);
    }

    #[test]
    fn applies_offset_correctly() {
        // Positive offset: lyrics appear earlier (subtract offset from timestamp)
        let positive_offset_lrc = "\
[offset: 500]
[00:10.00]Line 1
[00:20.00]Line 2";

        let lines = parse_lines(positive_offset_lrc);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].timestamp_seconds, 9.5);
        assert_eq!(lines[1].timestamp_seconds, 19.5);

        let timestamps = parse_timestamps(positive_offset_lrc);
        assert_eq!(timestamps, vec![9.5, 19.5]);

        // Negative offset: lyrics appear later (add offset to timestamp)
        let negative_offset_lrc = "\
[offset: -500]
[00:10.00]Line 1";
        let lines_neg = parse_lines(negative_offset_lrc);
        assert_eq!(lines_neg[0].timestamp_seconds, 10.5);

        // Clamping to 0.0
        let clamp_lrc = "\
[offset: 5000]
[00:02.00]Too early";
        let lines_clamped = parse_lines(clamp_lrc);
        assert_eq!(lines_clamped[0].timestamp_seconds, 0.0);
    }

    #[test]
    fn property_test_garbage_unicode_never_panics() {
        let mut state: u64 = 0x123456789abcdef0;
        let mut next_u32 = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 32) as u32
        };

        let sample_chars: [char; 26] = [
            '[',
            ']',
            ':',
            '.',
            '-',
            '+',
            ' ',
            '\t',
            '\n',
            '\r',
            '0',
            '1',
            '2',
            '9',
            'a',
            'r',
            't',
            'i',
            '🦀',
            '🎶',
            'ñ',
            '中',
            '\0',
            '\u{7f}',
            '\u{ffff}',
            '\u{10ffff}',
        ];

        for _ in 0..1000 {
            let len = (next_u32() % 64) as usize;
            let mut s = String::with_capacity(len);
            for _ in 0..len {
                let idx = (next_u32() as usize) % sample_chars.len();
                s.push(sample_chars[idx]);
            }

            // Must never panic on any arbitrary input
            let lines = parse_lines(&s);
            let timestamps = parse_timestamps(&s);
            let _ = parse_metadata(&s);
            let _ = is_synced(&s);
            let _ = audit_lrc(&s, Some(120.0), "Song");
            let _ = titles_match(&s, "Song");

            // Timestamps must be valid finite non-negative numbers
            for line in &lines {
                assert!(line.timestamp_seconds.is_finite());
                assert!(line.timestamp_seconds >= 0.0);
            }
            for &ts in &timestamps {
                assert!(ts.is_finite());
                assert!(ts >= 0.0);
            }
            // Lines should be in ascending timestamp order
            for window in lines.windows(2) {
                assert!(window[0].timestamp_seconds <= window[1].timestamp_seconds);
            }
            for window in timestamps.windows(2) {
                assert!(window[0] <= window[1]);
            }
        }
    }

    #[test]
    fn audits_short_timestamp_sets_as_not_downloadable() {
        assert_eq!(
            audit_lrc("[00:01]one\n[00:02]two", Some(3.0), ""),
            Some(LrcIssue::TooFewTimestamps)
        );
    }

    #[test]
    fn matches_titles_without_release_noise() {
        assert!(titles_match("Song (Live)", "song"));
    }
}
