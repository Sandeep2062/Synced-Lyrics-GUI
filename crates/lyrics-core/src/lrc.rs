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

const PAST_END_TOLERANCE: f64 = 5.0;
const EARLY_END_RATIO: f64 = 0.55;
const EARLY_END_GAP: f64 = 15.0;

pub fn parse_timestamps(text: &str) -> Vec<f64> {
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
                timestamps.push(seconds);
            }
            remaining = &remaining[end + 1..];
        }
    }
    timestamps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    timestamps
}

pub fn parse_lines(text: &str) -> Vec<LrcLine> {
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        let Some(close) = raw_line.find(']') else {
            continue;
        };
        let Some(timestamp_seconds) = parse_timestamp(&raw_line[1..close]) else {
            continue;
        };
        lines.push(LrcLine {
            timestamp_seconds,
            text: raw_line[close + 1..].trim().to_string(),
        });
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

pub fn titles_match(left: &str, right: &str) -> bool {
    let left = normalize_title(left);
    let right = normalize_title(right);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left.contains(&right) || right.contains(&left) || similarity(&left, &right) > 0.8
}

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
    if let Some(tag_title) = metadata_title(text) {
        if !title.is_empty() && !titles_match(title, tag_title) {
            return Some(LrcIssue::TitleMismatch);
        }
    }
    None
}

fn parse_timestamp(value: &str) -> Option<f64> {
    let (minutes, seconds) = value.split_once(':')?;
    let minutes = minutes.parse::<u64>().ok()?;
    let seconds = seconds.parse::<f64>().ok()?;
    if seconds >= 60.0 {
        return None;
    }
    Some(minutes as f64 * 60.0 + seconds)
}

fn metadata_title(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        let start = lower.find("[ti:")? + 4;
        let end = line[start..].find(']')? + start;
        Some(line[start..end].trim())
    })
}

fn normalize_title(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect()
}

fn similarity(left: &str, right: &str) -> f64 {
    let common = left
        .chars()
        .zip(right.chars())
        .filter(|(a, b)| a == b)
        .count();
    (common * 2) as f64 / (left.chars().count() + right.chars().count()) as f64
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
