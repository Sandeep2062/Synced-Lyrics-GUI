use crate::matching::Candidate;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LrcWord {
    pub timestamp_seconds: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LrcLine {
    pub timestamp_seconds: f64,
    pub text: String,
    pub words: Vec<LrcWord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LrcIssue {
    Empty,
    NotSynced,
    TooFewTimestamps,
    TooLong,
    EndsTooEarly,
    TitleMismatch,
    WrongSong,
    Offset(f64),
    WordsOutOfOrder,
}

impl std::fmt::Display for LrcIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "Empty lyrics"),
            Self::NotSynced => write!(f, "Not synced"),
            Self::TooFewTimestamps => write!(f, "Too few timestamps"),
            Self::TooLong => write!(f, "Lyrics exceed track duration"),
            Self::EndsTooEarly => write!(f, "Lyrics end too early"),
            Self::TitleMismatch => write!(f, "Title metadata mismatch"),
            Self::WrongSong => write!(f, "Wrong song detected"),
            Self::Offset(sec) => write!(f, "Offset: {:+.2}s", sec),
            Self::WordsOutOfOrder => write!(f, "Words or lines out of order"),
        }
    }
}

impl LrcIssue {
    pub fn reason_str(&self) -> String {
        self.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LrcAuditReport {
    pub issues: Vec<LrcIssue>,
    pub estimated_offset_seconds: Option<f64>,
    pub wrong_song_likelihood: f32,
    #[serde(default)]
    pub low_confidence_offset: bool,
}

impl Default for LrcAuditReport {
    fn default() -> Self {
        Self {
            issues: Vec::new(),
            estimated_offset_seconds: None,
            wrong_song_likelihood: 0.0,
            low_confidence_offset: false,
        }
    }
}

impl LrcAuditReport {
    pub fn is_valid(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn is_suspicious(&self) -> bool {
        self.issues
            .iter()
            .any(|i| !matches!(i, LrcIssue::Empty | LrcIssue::NotSynced))
    }

    pub fn primary_issue(&self) -> Option<&LrcIssue> {
        self.issues.first()
    }

    pub fn reason_summary(&self) -> Option<String> {
        let reasons: Vec<_> = self
            .issues
            .iter()
            .filter(|i| !matches!(i, LrcIssue::Empty | LrcIssue::NotSynced))
            .map(|i| i.to_string())
            .collect();
        if reasons.is_empty() {
            None
        } else {
            Some(reasons.join(", "))
        }
    }
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

/// Parse enhanced LRC word timestamps (<mm:ss.xx>) from line text.
///
/// Returns cleaned line text without the `<...>` timestamp tags, and a vector of `LrcWord`s
/// with their timestamps adjusted by `offset_seconds`.
/// If no word timestamps are present, returns `(raw_text.trim().to_string(), vec![])`.
pub fn parse_enhanced_words(
    raw_text: &str,
    line_ts: f64,
    offset_seconds: f64,
) -> (String, Vec<LrcWord>) {
    let mut words = Vec::new();
    let mut clean_text_parts = Vec::new();
    let mut remaining = raw_text;
    let mut current_ts: Option<f64> = None;
    let mut current_word_text = String::new();
    let mut has_word_timestamp = false;

    while let Some(start) = remaining.find('<') {
        let before_tag = &remaining[..start];
        if let Some(close_rel) = remaining[start..].find('>') {
            let close = start + close_rel;
            let tag_content = &remaining[start + 1..close];

            if let Some(ts) = parse_timestamp(tag_content) {
                has_word_timestamp = true;
                let combined_before = format!("{}{}", current_word_text, before_tag);
                let trimmed = combined_before.trim();
                if !trimmed.is_empty() {
                    let word_timestamp = current_ts.unwrap_or(line_ts);
                    words.push(LrcWord {
                        timestamp_seconds: word_timestamp,
                        text: trimmed.to_string(),
                    });
                    clean_text_parts.push(trimmed.to_string());
                }
                current_word_text.clear();
                let adjusted_ts = (ts - offset_seconds).max(0.0);
                current_ts = Some(adjusted_ts);

                remaining = &remaining[close + 1..];
                continue;
            }
        }
        // Not a timestamp tag or unclosed '<'
        current_word_text.push_str(&remaining[..start + 1]);
        remaining = &remaining[start + 1..];
    }

    current_word_text.push_str(remaining);
    let final_trimmed = current_word_text.trim();
    if !final_trimmed.is_empty() && has_word_timestamp {
        let ts = current_ts.unwrap_or(line_ts);
        words.push(LrcWord {
            timestamp_seconds: ts,
            text: final_trimmed.to_string(),
        });
        clean_text_parts.push(final_trimmed.to_string());
    }

    if has_word_timestamp {
        let clean = clean_text_parts.join(" ");
        (clean, words)
    } else {
        (raw_text.trim().to_string(), Vec::new())
    }
}

/// Find active line index using binary search ($O(\log N)$).
/// Returns -1 if no line has started yet or if lines is empty.
pub fn find_active_line_index(lines: &[LrcLine], current_time: f64) -> i32 {
    if lines.is_empty() || !current_time.is_finite() {
        return -1;
    }
    let idx = lines.partition_point(|line| line.timestamp_seconds <= current_time);
    if idx == 0 {
        -1
    } else {
        (idx - 1) as i32
    }
}

/// Find active word index using binary search ($O(\log N)$).
/// Returns -1 if no word has started yet or if words is empty.
pub fn find_active_word_index(words: &[LrcWord], current_time: f64) -> i32 {
    if words.is_empty() || !current_time.is_finite() {
        return -1;
    }
    let idx = words.partition_point(|word| word.timestamp_seconds <= current_time);
    if idx == 0 {
        -1
    } else {
        (idx - 1) as i32
    }
}

/// Parse lyrics lines and timestamps from LRC text into sorted `LrcLine` items.
///
/// Features:
/// - Panic-free on any input (empty strings, leading whitespace, multi-byte characters, unmatched brackets).
/// - Supports multiple timestamps per line (e.g. `[00:10][00:50]text` produces two `LrcLine` entries).
/// - Applies `[offset:]` metadata (positive = lyrics appear earlier, clamped to 0.0).
/// - Parses enhanced LRC word timestamps `<mm:ss.xx>` when present.
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

        let first_ts = (line_timestamps[0] - offset_seconds).max(0.0);
        let (lyric_text, words) = parse_enhanced_words(remaining, first_ts, offset_seconds);
        for ts in line_timestamps {
            let adjusted = (ts - offset_seconds).max(0.0);
            lines.push(LrcLine {
                timestamp_seconds: adjusted,
                text: lyric_text.clone(),
                words: words.clone(),
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

pub fn audit_lrc(
    text: &str,
    duration_seconds: Option<f64>,
    title: &str,
    candidates: &[Candidate],
) -> LrcAuditReport {
    let mut issues = Vec::new();
    let mut estimated_offset_seconds: Option<f64> = None;
    let mut wrong_song_likelihood: f32 = 0.0;
    let mut low_confidence_offset = false;

    if text.trim().is_empty() {
        issues.push(LrcIssue::Empty);
        return LrcAuditReport {
            issues,
            estimated_offset_seconds: None,
            wrong_song_likelihood: 0.0,
            low_confidence_offset: false,
        };
    }

    let timestamps = parse_timestamps(text);
    if timestamps.is_empty() {
        issues.push(LrcIssue::NotSynced);
        return LrcAuditReport {
            issues,
            estimated_offset_seconds: None,
            wrong_song_likelihood: 0.0,
            low_confidence_offset: false,
        };
    }

    if timestamps.len() < 3 {
        issues.push(LrcIssue::TooFewTimestamps);
    }

    if let Some(duration) = duration_seconds.filter(|duration| *duration > 0.0) {
        let last = *timestamps.last().unwrap();
        if last > duration + PAST_END_TOLERANCE {
            issues.push(LrcIssue::TooLong);
        }
        if last < duration * EARLY_END_RATIO && duration - last > EARLY_END_GAP {
            issues.push(LrcIssue::EndsTooEarly);
        }
    }

    if let Some(ref tag_title) = metadata_title(text) {
        if !title.is_empty() && !titles_match(title, tag_title) {
            issues.push(LrcIssue::TitleMismatch);
        }
    }

    // Check for out-of-order timestamps in raw lines (WordsOutOfOrder)
    let mut prev_ts: Option<f64> = None;
    let mut has_out_of_order = false;
    for line in text.lines() {
        let mut rem = line.trim_start();
        while rem.starts_with('[') {
            let Some(close) = rem.find(']') else { break };
            let tag = &rem[1..close];
            if let Some(ts) = parse_timestamp(tag) {
                if let Some(prev) = prev_ts {
                    if ts < prev - 0.5 {
                        has_out_of_order = true;
                    }
                }
                prev_ts = Some(ts);
                rem = rem[close + 1..].trim_start();
            } else {
                break;
            }
        }
    }
    // Also check intra-line word timestamps <mm:ss.xx>
    for line in text.lines() {
        let mut rem = line;
        while let Some(start) = rem.find('<') {
            let Some(end_offset) = rem[start..].find('>') else {
                break;
            };
            let tag = &rem[start + 1..start + end_offset];
            if let Some(ts) = parse_timestamp(tag) {
                if let Some(prev) = prev_ts {
                    if ts < prev - 0.5 {
                        has_out_of_order = true;
                    }
                }
                prev_ts = Some(ts);
            }
            rem = &rem[start + end_offset + 1..];
        }
    }
    if has_out_of_order {
        issues.push(LrcIssue::WordsOutOfOrder);
    }

    // Wrong song comparison against fetched candidates
    let existing_clean = clean_lyrics_text(text);
    let existing_tokens = tokenize_lyrics(&existing_clean);

    let cand_with_lyrics: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.synced.is_some() || c.plain.is_some())
        .collect();

    if !cand_with_lyrics.is_empty() && existing_tokens.len() >= 5 {
        let mut max_overlap = 0.0f64;
        for cand in &cand_with_lyrics {
            if let Some(cand_text) = cand.best_text() {
                let cand_clean = clean_lyrics_text(cand_text);
                let cand_tokens = tokenize_lyrics(&cand_clean);
                if cand_tokens.len() >= 5 {
                    let overlap = compute_token_overlap(&existing_tokens, &cand_tokens);
                    if overlap > max_overlap {
                        max_overlap = overlap;
                    }
                }
            }
        }

        if max_overlap < 0.20 {
            wrong_song_likelihood = (1.0 - (max_overlap / 0.20)).clamp(0.6, 1.0) as f32;
            issues.push(LrcIssue::WrongSong);
        } else if max_overlap < 0.35 {
            wrong_song_likelihood = ((0.35 - max_overlap) / 0.35 * 0.5) as f32;
        } else {
            wrong_song_likelihood = 0.0;
        }
    }

    // Offset detection: align existing LRC against best fetched synced candidate
    let best_synced_candidate = candidates
        .iter()
        .filter(|c| c.synced.is_some())
        .max_by(|a, b| {
            let a_sim = similarity(&a.title, title);
            let b_sim = similarity(&b.title, title);
            a_sim.partial_cmp(&b_sim).unwrap_or(Ordering::Equal)
        });

    if let Some(ref_cand) = best_synced_candidate {
        let ref_text = ref_cand.synced.as_deref().unwrap_or("");
        let existing_lines = parse_lines(text);
        let ref_lines = parse_lines(ref_text);

        let existing_lines: Vec<_> = existing_lines
            .into_iter()
            .filter(|l| !l.text.trim().is_empty())
            .collect();
        let ref_lines: Vec<_> = ref_lines
            .into_iter()
            .filter(|l| !l.text.trim().is_empty())
            .collect();

        let mut diffs = Vec::new();
        let mut matched_indices = Vec::new();

        for (e_idx, e_line) in existing_lines.iter().enumerate() {
            let e_norm = normalize_line_text(&e_line.text);
            if e_norm.is_empty() {
                continue;
            }
            let mut best_ref_match: Option<(usize, f64)> = None;
            for (r_idx, r_line) in ref_lines.iter().enumerate() {
                let r_norm = normalize_line_text(&r_line.text);
                if r_norm.is_empty() {
                    continue;
                }
                let score = if e_norm == r_norm {
                    1.0
                } else {
                    similarity(&e_norm, &r_norm)
                };
                if score >= 0.80 {
                    let ts_dist = (e_line.timestamp_seconds - r_line.timestamp_seconds).abs();
                    match best_ref_match {
                        None => best_ref_match = Some((r_idx, score)),
                        Some((prev_idx, prev_score)) => {
                            let prev_dist = (e_line.timestamp_seconds
                                - ref_lines[prev_idx].timestamp_seconds)
                                .abs();
                            if score > prev_score + 0.1
                                || (score >= prev_score - 0.05 && ts_dist < prev_dist)
                            {
                                best_ref_match = Some((r_idx, score));
                            }
                        }
                    }
                }
            }

            if let Some((r_idx, _)) = best_ref_match {
                let diff = e_line.timestamp_seconds - ref_lines[r_idx].timestamp_seconds;
                diffs.push(diff);
                matched_indices.push((e_idx, r_idx));
            }
        }

        // Check for out-of-order sequence compared to reference
        if matched_indices.len() >= 4 {
            let mut inversions = 0;
            for w in matched_indices.windows(2) {
                if w[0].1 > w[1].1 {
                    inversions += 1;
                }
            }
            if inversions >= 2 && !issues.contains(&LrcIssue::WordsOutOfOrder) {
                issues.push(LrcIssue::WordsOutOfOrder);
            }
        }

        if diffs.len() >= 3 {
            diffs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
            let mid = diffs.len() / 2;
            let median = if diffs.len() % 2 == 0 {
                (diffs[mid - 1] + diffs[mid]) / 2.0
            } else {
                diffs[mid]
            };

            let q1 = diffs[diffs.len() / 4];
            let q3 = diffs[diffs.len() * 3 / 4];
            let spread = (q3 - q1).abs();

            if spread <= 0.6 && median.abs() > 0.5 {
                estimated_offset_seconds = Some(median);
                issues.push(LrcIssue::Offset(median));
            }
        }
    } else {
        // Fall back to heuristics only if no reference exists, and mark that as low-confidence.
        let lines = parse_lines(text);
        let meta = parse_metadata(text);

        // Heuristic 1: first line starts before 0 (e.g. unadjusted raw timestamp minus offset < -0.5s)
        let mut first_starts_before_zero = false;
        let mut est_heuristic_offset = None;
        for raw_line in text.lines() {
            let trimmed = raw_line.trim_start();
            if trimmed.starts_with('[') {
                if let Some(close) = trimmed.find(']') {
                    let tag = &trimmed[1..close];
                    if let Some(ts) = parse_timestamp(tag) {
                        let unadjusted = ts - meta.offset_seconds();
                        if unadjusted < -0.5 {
                            first_starts_before_zero = true;
                            est_heuristic_offset = Some(unadjusted);
                        }
                        break;
                    }
                }
            }
        }

        // Heuristic 2: long silence after the intro (e.g. line 0 starts <= 2.0s and line 1 is > 25.0s later)
        let mut intro_silence_offset = None;
        if lines.len() >= 2 {
            let t0 = lines[0].timestamp_seconds;
            let t1 = lines[1].timestamp_seconds;
            if t0 <= 2.0 && t1 - t0 > 25.0 {
                intro_silence_offset = Some(t1 - 15.0);
            }
        }

        if first_starts_before_zero {
            if let Some(offset) = est_heuristic_offset {
                if offset.abs() > 0.5 {
                    estimated_offset_seconds = Some(offset);
                    issues.push(LrcIssue::Offset(offset));
                    low_confidence_offset = true;
                }
            }
        } else if let Some(offset) = intro_silence_offset {
            if offset.abs() > 0.5 {
                estimated_offset_seconds = Some(offset);
                issues.push(LrcIssue::Offset(offset));
                low_confidence_offset = true;
            }
        }
    }

    LrcAuditReport {
        issues,
        estimated_offset_seconds,
        wrong_song_likelihood,
        low_confidence_offset,
    }
}

/// Write or update the `[offset: ms]` metadata tag in the LRC text.
///
/// Positive `offset_seconds` shifts lyrics earlier (subtracts from raw timestamp).
/// If `[offset: X]` tag already exists, adjusts the existing value.
/// If not, inserts `[offset: ms]` into the file.
pub fn apply_offset_tag(text: &str, offset_seconds: f64) -> String {
    let delta_ms = (offset_seconds * 1000.0).round() as i64;
    let mut replaced = false;
    let mut out_lines = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[offset:") && trimmed.ends_with(']') {
            let inner = trimmed[8..trimmed.len() - 1].trim();
            let clean = inner.replace(' ', "");
            let current_ms = clean.parse::<i64>().unwrap_or(0);
            let new_ms = current_ms + delta_ms;
            out_lines.push(format!("[offset: {}]", new_ms));
            replaced = true;
        } else {
            out_lines.push(line.to_string());
        }
    }

    if !replaced {
        let mut insert_idx = 0;
        for (i, line) in out_lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('[')
                && (trimmed.starts_with("[ar:")
                    || trimmed.starts_with("[ti:")
                    || trimmed.starts_with("[al:")
                    || trimmed.starts_with("[by:"))
            {
                insert_idx = i + 1;
            } else if !trimmed.is_empty() && !trimmed.starts_with('[') {
                break;
            }
        }
        out_lines.insert(insert_idx, format!("[offset: {}]", delta_ms));
    }

    out_lines.join("\n")
}

/// Rewrite all timestamps in the LRC text by subtracting `offset_seconds`, clamping to 0.0.
/// Any existing `[offset: ...]` tag is stripped to avoid double-adjusting.
pub fn rewrite_timestamps_with_offset(text: &str, offset_seconds: f64) -> String {
    let mut out_lines = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[offset:") && trimmed.ends_with(']') {
            continue;
        }

        let mut remaining = line;
        let mut new_line = String::new();
        let mut has_timestamp = false;

        while let Some(start) = remaining.find('[') {
            let Some(close_offset) = remaining[start..].find(']') else {
                break;
            };
            let close = start + close_offset;
            let tag = &remaining[start + 1..close];

            if let Some(ts) = parse_timestamp(tag) {
                has_timestamp = true;
                new_line.push_str(&remaining[..start]);
                let adjusted = (ts - offset_seconds).max(0.0);
                let minutes = (adjusted / 60.0).floor() as u64;
                let seconds = adjusted - (minutes as f64 * 60.0);
                new_line.push_str(&format!("[{:02}:{:05.2}]", minutes, seconds));
                remaining = &remaining[close + 1..];
            } else {
                new_line.push_str(&remaining[..=close]);
                remaining = &remaining[close + 1..];
            }
        }
        new_line.push_str(remaining);

        out_lines.push(if has_timestamp {
            new_line
        } else {
            line.to_string()
        });
    }

    out_lines.join("\n")
}

fn clean_lyrics_text(text: &str) -> String {
    let mut result = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.contains(':') {
            let inner = &trimmed[1..trimmed.len() - 1];
            if inner.starts_with("ar:")
                || inner.starts_with("ti:")
                || inner.starts_with("al:")
                || inner.starts_with("by:")
                || inner.starts_with("offset:")
                || inner.starts_with("length:")
            {
                continue;
            }
        }
        let mut in_bracket = false;
        let mut in_angle = false;
        for c in line.chars() {
            if c == '[' {
                in_bracket = true;
            } else if c == ']' {
                in_bracket = false;
            } else if c == '<' {
                in_angle = true;
            } else if c == '>' {
                in_angle = false;
            } else if !in_bracket && !in_angle {
                result.push(c);
            }
        }
        result.push('\n');
    }
    result
}

fn tokenize_lyrics(text: &str) -> std::collections::HashSet<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|w| w.len() >= 2)
        .collect()
}

fn compute_token_overlap(
    set_a: &std::collections::HashSet<String>,
    set_b: &std::collections::HashSet<String>,
) -> f64 {
    if set_a.is_empty() || set_b.is_empty() {
        return 0.0;
    }
    let intersection = set_a.intersection(set_b).count();
    let union = set_a.union(set_b).count();
    let jaccard = intersection as f64 / union as f64;
    let containment = intersection as f64 / set_a.len().min(set_b.len()) as f64;
    jaccard.max(containment * 0.7)
}

fn normalize_line_text(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
            let _ = audit_lrc(&s, Some(120.0), "Song", &[]);
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
        let report = audit_lrc("[00:01]one\n[00:02]two", Some(3.0), "", &[]);
        assert!(report.issues.contains(&LrcIssue::TooFewTimestamps));
    }

    #[test]
    fn matches_titles_without_release_noise() {
        assert!(titles_match("Song (Live)", "song"));
    }

    #[test]
    fn test_synthetic_lrc_shifted_plus_two_seconds() {
        let ref_lrc = "\
[00:10.00]First line of the song
[00:15.00]Second line of the song
[00:20.00]Third line of the song
[00:25.00]Fourth line of the song
[00:30.00]Fifth line of the song";

        let candidate = Candidate::new("My Song", "My Artist", None, Some(60.0)).with_lyrics(
            Some(ref_lrc.to_string()),
            None,
            crate::providers::ProviderName::Lrclib,
        );

        let shifted_lrc = "\
[00:12.00]First line of the song
[00:17.00]Second line of the song
[00:22.00]Third line of the song
[00:27.00]Fourth line of the song
[00:32.00]Fifth line of the song";

        let report = audit_lrc(shifted_lrc, Some(60.0), "My Song", &[candidate]);

        assert!(report
            .issues
            .iter()
            .any(|i| matches!(i, LrcIssue::Offset(val) if (*val - 2.0).abs() < 0.05)));
        assert_eq!(
            report
                .estimated_offset_seconds
                .map(|v| (v * 100.0).round() / 100.0),
            Some(2.0)
        );
        assert!(report.wrong_song_likelihood < 0.2);
        assert!(!report.low_confidence_offset);

        // Test Fix Offset with [offset:] tag
        let fixed_tag = apply_offset_tag(shifted_lrc, 2.0);
        let parsed_tag_lines = parse_lines(&fixed_tag);
        assert_eq!(parsed_tag_lines.len(), 5);
        assert!((parsed_tag_lines[0].timestamp_seconds - 10.0).abs() < 0.05);
        assert!((parsed_tag_lines[1].timestamp_seconds - 15.0).abs() < 0.05);

        // Test Fix Offset with rewriting timestamps
        let fixed_rw = rewrite_timestamps_with_offset(shifted_lrc, 2.0);
        let parsed_rw_lines = parse_lines(&fixed_rw);
        assert_eq!(parsed_rw_lines.len(), 5);
        assert!((parsed_rw_lines[0].timestamp_seconds - 10.0).abs() < 0.05);
        assert!((parsed_rw_lines[1].timestamp_seconds - 15.0).abs() < 0.05);
    }

    #[test]
    fn test_synthetic_lrc_shifted_minus_one_point_five_seconds() {
        let ref_lrc = "\
[00:10.00]First line of the song
[00:15.00]Second line of the song
[00:20.00]Third line of the song
[00:25.00]Fourth line of the song
[00:30.00]Fifth line of the song";

        let candidate = Candidate::new("My Song", "My Artist", None, Some(60.0)).with_lyrics(
            Some(ref_lrc.to_string()),
            None,
            crate::providers::ProviderName::Lrclib,
        );

        let shifted_lrc = "\
[00:08.50]First line of the song
[00:13.50]Second line of the song
[00:18.50]Third line of the song
[00:23.50]Fourth line of the song
[00:28.50]Fifth line of the song";

        let report = audit_lrc(shifted_lrc, Some(60.0), "My Song", &[candidate]);

        assert!(report
            .issues
            .iter()
            .any(|i| matches!(i, LrcIssue::Offset(val) if (*val - (-1.5)).abs() < 0.05)));
        assert_eq!(
            report
                .estimated_offset_seconds
                .map(|v| (v * 100.0).round() / 100.0),
            Some(-1.5)
        );
        assert!(report.wrong_song_likelihood < 0.2);

        // Test Fix Offset with [offset:] tag
        let fixed_tag = apply_offset_tag(shifted_lrc, -1.5);
        let parsed_tag_lines = parse_lines(&fixed_tag);
        assert_eq!(parsed_tag_lines.len(), 5);
        assert!((parsed_tag_lines[0].timestamp_seconds - 10.0).abs() < 0.05);
        assert!((parsed_tag_lines[1].timestamp_seconds - 15.0).abs() < 0.05);

        // Test Fix Offset with rewriting timestamps
        let fixed_rw = rewrite_timestamps_with_offset(shifted_lrc, -1.5);
        let parsed_rw_lines = parse_lines(&fixed_rw);
        assert_eq!(parsed_rw_lines.len(), 5);
        assert!((parsed_rw_lines[0].timestamp_seconds - 10.0).abs() < 0.05);
        assert!((parsed_rw_lines[1].timestamp_seconds - 15.0).abs() < 0.05);
    }

    #[test]
    fn test_synthetic_lrc_totally_different_song() {
        let candidate_lrc = "\
[00:10.00]Is this the real life
[00:15.00]Is this just fantasy
[00:20.00]Caught in a landslide
[00:25.00]No escape from reality
[00:30.00]Open your eyes";

        let candidate = Candidate::new("Bohemian Rhapsody", "Queen", None, Some(180.0))
            .with_lyrics(
                Some(candidate_lrc.to_string()),
                None,
                crate::providers::ProviderName::Lrclib,
            );

        let totally_different_lrc = "\
[00:10.00]Ground control to Major Tom
[00:15.00]Ground control to Major Tom
[00:20.00]Take your protein pills
[00:25.00]And put your helmet on
[00:30.00]Ten nine eight seven";

        let report = audit_lrc(
            totally_different_lrc,
            Some(180.0),
            "Bohemian Rhapsody",
            &[candidate],
        );

        assert!(report.issues.contains(&LrcIssue::WrongSong));
        assert!(report.wrong_song_likelihood >= 0.7);
    }

    #[test]
    fn test_words_out_of_order() {
        let backwards_lrc = "\
[00:20.00]Second line appearing first
[00:10.00]First line appearing second
[00:30.00]Third line appearing third";

        let report = audit_lrc(backwards_lrc, Some(60.0), "My Song", &[]);
        assert!(report.issues.contains(&LrcIssue::WordsOutOfOrder));
    }

    #[test]
    fn test_enhanced_lrc_word_timestamps_parsing() {
        let enhanced_lrc = "\
[00:10.00]<00:10.00>The <00:10.50>quick <00:11.00>brown <00:11.80>fox
[00:15.00]Standard line without word tags
[00:20.00] <00:20.00> Another <00:20.40> line <00:21.00>";

        let lines = parse_lines(enhanced_lrc);
        assert_eq!(lines.len(), 3);

        // Line 0: Enhanced with words
        assert_eq!(lines[0].timestamp_seconds, 10.0);
        assert_eq!(lines[0].text, "The quick brown fox");
        assert_eq!(lines[0].words.len(), 4);
        assert_eq!(lines[0].words[0].text, "The");
        assert!((lines[0].words[0].timestamp_seconds - 10.0).abs() < 1e-4);
        assert_eq!(lines[0].words[1].text, "quick");
        assert!((lines[0].words[1].timestamp_seconds - 10.5).abs() < 1e-4);
        assert_eq!(lines[0].words[2].text, "brown");
        assert!((lines[0].words[2].timestamp_seconds - 11.0).abs() < 1e-4);
        assert_eq!(lines[0].words[3].text, "fox");
        assert!((lines[0].words[3].timestamp_seconds - 11.8).abs() < 1e-4);

        // Line 1: Standard line, falls back to empty words (line-level)
        assert_eq!(lines[1].timestamp_seconds, 15.0);
        assert_eq!(lines[1].text, "Standard line without word tags");
        assert!(lines[1].words.is_empty());

        // Line 2: Enhanced with trailing tag
        assert_eq!(lines[2].timestamp_seconds, 20.0);
        assert_eq!(lines[2].text, "Another line");
        assert_eq!(lines[2].words.len(), 2);
        assert_eq!(lines[2].words[0].text, "Another");
        assert!((lines[2].words[0].timestamp_seconds - 20.0).abs() < 1e-4);
        assert_eq!(lines[2].words[1].text, "line");
        assert!((lines[2].words[1].timestamp_seconds - 20.4).abs() < 1e-4);
    }

    #[test]
    fn test_enhanced_lrc_with_offset_tag() {
        let lrc_with_offset = "\
[offset: 500]
[00:10.00]<00:10.00>Hello <00:10.50>world";

        let lines = parse_lines(lrc_with_offset);
        assert_eq!(lines.len(), 1);
        // Offset +500ms shifts lyrics 0.5s earlier: 10.0 - 0.5 = 9.5
        assert!((lines[0].timestamp_seconds - 9.5).abs() < 1e-4);
        assert_eq!(lines[0].words.len(), 2);
        assert!((lines[0].words[0].timestamp_seconds - 9.5).abs() < 1e-4);
        assert!((lines[0].words[1].timestamp_seconds - 10.0).abs() < 1e-4);
    }

    #[test]
    fn test_enhanced_lrc_non_timestamp_brackets() {
        let text = "[00:10.00]Guitar solo <Guitar Solo> ends";
        let lines = parse_lines(text);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Guitar solo <Guitar Solo> ends");
        assert!(
            lines[0].words.is_empty(),
            "Should not treat non-timestamps as words"
        );
    }

    #[test]
    fn test_binary_search_active_line_and_words() {
        let lrc = "\
[00:10.00]<00:10.00>WordA <00:12.00>WordB <00:14.00>WordC
[00:20.00]Second line plain
[00:30.00]<00:30.00>Last <00:32.00>line";

        let lines = parse_lines(lrc);

        // Before first line
        assert_eq!(find_active_line_index(&lines, 0.0), -1);
        assert_eq!(find_active_line_index(&lines, 9.99), -1);

        // Exactly at first line
        assert_eq!(find_active_line_index(&lines, 10.0), 0);
        // During first line
        assert_eq!(find_active_line_index(&lines, 15.0), 0);
        assert_eq!(find_active_line_index(&lines, 19.99), 0);

        // At second line
        assert_eq!(find_active_line_index(&lines, 20.0), 1);
        assert_eq!(find_active_line_index(&lines, 25.0), 1);

        // At third line
        assert_eq!(find_active_line_index(&lines, 30.0), 2);
        // Past the end
        assert_eq!(find_active_line_index(&lines, 100.0), 2);

        // Test word binary search on line 0
        let words = &lines[0].words;
        assert_eq!(find_active_word_index(words, 9.99), -1);
        assert_eq!(find_active_word_index(words, 10.0), 0);
        assert_eq!(find_active_word_index(words, 11.5), 0);
        assert_eq!(find_active_word_index(words, 12.0), 1);
        assert_eq!(find_active_word_index(words, 13.9), 1);
        assert_eq!(find_active_word_index(words, 14.0), 2);
        assert_eq!(find_active_word_index(words, 18.0), 2);

        // Test consistency with linear scan across 1000 seek positions
        for t_int in 0..1000 {
            let t = t_int as f64 * 0.05; // 0.0s to 50.0s in 50ms steps
            let binary_idx = find_active_line_index(&lines, t);
            let linear_idx = lines
                .iter()
                .enumerate()
                .rev()
                .find(|(_, l)| l.timestamp_seconds <= t)
                .map(|(i, _)| i as i32)
                .unwrap_or(-1);
            assert_eq!(
                binary_idx, linear_idx,
                "Binary search must match linear scan exactly at t={t}s"
            );
        }
    }
}
