use crate::model::Track;
use crate::providers::ProviderName;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

/// Represents a candidate lyrics/track match from an external provider.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Candidate {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_seconds: Option<f64>,
    pub synced: Option<String>,
    pub plain: Option<String>,
    pub provider: ProviderName,
}

impl Candidate {
    pub fn new(
        title: impl Into<String>,
        artist: impl Into<String>,
        album: Option<String>,
        duration_seconds: Option<f64>,
    ) -> Self {
        Self {
            title: title.into(),
            artist: artist.into(),
            album,
            duration_seconds,
            synced: None,
            plain: None,
            provider: ProviderName::Lrclib,
        }
    }

    pub fn with_lyrics(
        mut self,
        synced: Option<String>,
        plain: Option<String>,
        provider: ProviderName,
    ) -> Self {
        self.synced = synced;
        self.plain = plain;
        self.provider = provider;
        self
    }

    pub fn best_text(&self) -> Option<&str> {
        self.synced.as_deref().or(self.plain.as_deref())
    }

    pub fn is_synced(&self) -> bool {
        self.synced.is_some()
    }
}

/// Detailed similarity scores across matching dimensions (all scores normalized between 0.0 and 1.0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchScore {
    pub title: f64,
    pub artist: f64,
    pub album: f64,
    pub duration: f64,
    pub total: f64,
}

impl MatchScore {
    /// Returns true if the score meets high-confidence match criteria.
    pub fn is_match(&self) -> bool {
        self.total >= 0.75 && self.title >= 0.65 && self.artist >= 0.50 && self.duration >= 0.20
    }
}

/// Data-driven noise patterns for bracketed metadata tags: `(...)`, `[...]`, `{...}`.
const BRACKETED_NOISE_PREFIXES: &[&str] = &[
    "feat.",
    "feat ",
    "ft.",
    "ft ",
    "ft,",
    "featuring",
    "with ",
    "prod. by",
    "prod by",
    "prod.",
    "prod ",
    "produced by",
    "produced",
    "official video",
    "official audio",
    "official music video",
    "official hd",
    "official 4k",
    "official visualizer",
    "music video",
    "lyric video",
    "lyrics video",
    "visualizer",
    "audio",
    "video",
    "remastered",
    "remaster",
    "live at",
    "live from",
    "live in",
    "live version",
    "live session",
    "live",
    "deluxe edition",
    "deluxe version",
    "deluxe",
    "expanded edition",
    "explicit version",
    "explicit",
    "clean version",
    "clean",
    "single version",
    "album version",
    "radio edit",
    "original mix",
    "extended mix",
    "instrumental",
    "acoustic version",
    "acoustic",
];

/// Data-driven noise patterns for trailing hyphen/slash separated release tags.
const SUFFIX_NOISE_PATTERNS: &[&str] = &[
    "single version",
    "album version",
    "radio edit",
    "remastered",
    "remaster",
    "live version",
    "live",
    "explicit version",
    "explicit",
    "clean version",
    "clean",
    "official video",
    "official audio",
    "instrumental",
];

/// Strip common noise suffixes, production notes, features, release tags, and track numbers.
pub fn clean_title(title: &str) -> String {
    let mut s = title.trim();

    // Strip leading track number prefixes like "01. ", "1. ", "01 - ", "1.) ", "[E] "
    if let Some(rest) = s.strip_prefix("[E]") {
        s = rest.trim();
    }
    if let Some(idx) = s.find(|c: char| !c.is_ascii_digit()) {
        let digits = &s[..idx];
        if !digits.is_empty() {
            let tail = &s[idx..];
            if tail.starts_with(". ") || tail.starts_with(" - ") || tail.starts_with(".) ") {
                if let Some(pos) = tail.find(' ') {
                    s = tail[pos + 1..].trim();
                }
            }
        }
    }

    // Strip bracketed noise: (feat. ...), [Prod By ...], (Official Video), (2015), (Explicit), etc.
    let mut cleaned = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '(' || c == '[' || c == '{' {
            let close = match c {
                '(' => ')',
                '[' => ']',
                '{' => '}',
                _ => ')',
            };
            let mut inside = String::new();
            let mut found_close = false;
            for ch in chars.by_ref() {
                if ch == close {
                    found_close = true;
                    break;
                }
                inside.push(ch);
            }
            if found_close {
                let inside_trimmed = inside.trim();
                let lower = inside_trimmed.to_ascii_lowercase();

                // Check if bracketed content matches known noise prefixes or 4-digit years
                let is_noise = lower.is_empty()
                    || is_four_digit_year(&lower)
                    || is_remaster_year(&lower)
                    || BRACKETED_NOISE_PREFIXES
                        .iter()
                        .any(|&p| lower == p || lower.starts_with(p));

                if !is_noise {
                    // Retain non-noise bracketed text (e.g. valid subtitle)
                    cleaned.push(' ');
                    cleaned.push_str(inside_trimmed);
                    cleaned.push(' ');
                }
            } else {
                cleaned.push(c);
                cleaned.push_str(&inside);
            }
        } else {
            cleaned.push(c);
        }
    }

    let mut result = cleaned.trim().to_string();

    // Strip inline unbracketed features and producers:
    // e.g. "Passion ft Scr1bz", "On My Own ft. ColdSiemens", "With It Ft. Mackned & Wicca Phase Springs Eternal",
    // "On The Run Ft, Lil Peep", "Passion prod Greaf"
    result = strip_inline_feature_or_producer(&result);

    // Strip trailing "- Single Version", "- Remastered", etc.
    result = strip_suffix_noise(&result);

    result.trim().to_string()
}

fn is_four_digit_year(s: &str) -> bool {
    s.len() == 4
        && s.chars().all(|c| c.is_ascii_digit())
        && (s.starts_with("19") || s.starts_with("20"))
}

fn is_remaster_year(s: &str) -> bool {
    // Matches patterns like "2015 remaster" or "remaster 2011"
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() == 2 {
        (is_four_digit_year(parts[0]) && parts[1].starts_with("remaster"))
            || (parts[0].starts_with("remaster") && is_four_digit_year(parts[1]))
    } else {
        false
    }
}

fn strip_inline_feature_or_producer(s: &str) -> String {
    // Look for word boundaries followed by ft, ft., feat, feat., featuring, prod, prod by, produced by
    let keywords = [
        " ft. ",
        " ft ",
        " ft, ",
        " feat. ",
        " feat ",
        " featuring ",
        " prod by ",
        " prod. by ",
        " produced by ",
        " prod. ",
        " prod ",
    ];

    let mut cut_index = None;
    for (byte_idx, _) in s.char_indices() {
        let suffix = &s[byte_idx..];
        for kw in &keywords {
            if suffix
                .get(..kw.len())
                .is_some_and(|sub| sub.eq_ignore_ascii_case(kw))
            {
                cut_index = Some(cut_index.map_or(byte_idx, |cur: usize| cur.min(byte_idx)));
            }
        }
    }

    if let Some(idx) = cut_index {
        s[..idx].trim().to_string()
    } else {
        s.to_string()
    }
}

fn strip_suffix_noise(s: &str) -> String {
    let mut cur = s.trim();
    for sep in &[" - ", " / "] {
        if let Some((head, tail)) = cur.rsplit_once(sep) {
            let tail_lower = tail.trim().to_ascii_lowercase();
            let is_noise = is_remaster_year(&tail_lower)
                || is_four_digit_year(&tail_lower)
                || SUFFIX_NOISE_PATTERNS
                    .iter()
                    .any(|&p| tail_lower == p || tail_lower.starts_with(p));
            if is_noise {
                cur = head.trim();
            }
        }
    }
    cur.to_string()
}

/// Unicode-aware normalization:
/// - NFKD decomposition
/// - Strips combining diacritical marks (e.g. `é` -> `e`, `ö` -> `o`, `ñ` -> `n`)
/// - Full Unicode case folding
/// - Retains non-Latin scripts (CJK, Cyrillic, Devanagari, Arabic, Greek, etc.)
/// - Strips punctuation, collapses consecutive whitespace
pub fn normalize_title(value: &str) -> String {
    // 1. NFKD decomposition + diacritic stripping
    let decomposed: String = value
        .nfkd()
        .filter(|c| !is_combining_diacritic(*c))
        .collect();

    // 2. Unicode case-folding
    let lower = decomposed.to_lowercase();

    // 3. Keep alphanumeric characters, whitespace, and non-Latin combining marks (e.g. Devanagari matras)
    let mut normalized = String::with_capacity(lower.len());
    let mut last_was_space = false;

    for c in lower.chars() {
        if c.is_alphanumeric() || is_non_latin_script_mark(c) {
            normalized.push(c);
            last_was_space = false;
        } else if (c.is_whitespace() || c == '-' || c == '_' || c == '/')
            && !last_was_space
            && !normalized.is_empty()
        {
            normalized.push(' ');
            last_was_space = true;
        }
    }

    normalized.trim_end().to_string()
}

fn is_combining_diacritic(c: char) -> bool {
    matches!(
        c,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

fn is_non_latin_script_mark(c: char) -> bool {
    matches!(
        c,
        '\u{0591}'..='\u{05C7}' // Hebrew cantillation / points
            | '\u{0610}'..='\u{065F}' | '\u{0670}' // Arabic harakat
            | '\u{0900}'..='\u{0D7F}' // Indic scripts: Devanagari, Bengali, Tamil, etc.
            | '\u{0E31}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}' // Thai
            | '\u{0EB1}'..='\u{0EBA}' | '\u{0EBB}'..='\u{0ECC}' // Lao
            | '\u{3099}'..='\u{309A}' // Japanese voiced / semi-voiced marks
    )
}

/// Compute string similarity between 0.0 and 1.0 using Jaro-Winkler and Token-Set Ratio.
pub fn similarity(left: &str, right: &str) -> f64 {
    let left_clean = clean_title(left);
    let right_clean = clean_title(right);

    let left_norm = normalize_title(&left_clean);
    let right_norm = normalize_title(&right_clean);

    if left_norm.is_empty() && right_norm.is_empty() {
        return 1.0;
    }
    if left_norm.is_empty() || right_norm.is_empty() {
        return 0.0;
    }
    if left_norm == right_norm {
        return 1.0;
    }

    let jw = jaro_winkler(&left_norm, &right_norm);
    let tsr = token_set_ratio(&left_norm, &right_norm);
    let norm_lev = normalized_levenshtein(&left_norm, &right_norm);

    let mut sim = jw.max(tsr).max(norm_lev);

    // If both strings contain standalone numbers and they differ (e.g. "2 Am" vs "3 Am",
    // "Part 1" vs "Part 2"), penalize similarity heavily since numbers denote different tracks.
    let nums_left: Vec<&str> = left_norm
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .collect();
    let nums_right: Vec<&str> = right_norm
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .collect();

    if !nums_left.is_empty() && !nums_right.is_empty() && nums_left != nums_right {
        sim *= 0.40;
    }

    sim.clamp(0.0, 1.0)
}

/// Check if two titles match based on normalization and similarity threshold.
pub fn titles_match(left: &str, right: &str) -> bool {
    let left_clean = clean_title(left);
    let right_clean = clean_title(right);

    let left_norm = normalize_title(&left_clean);
    let right_norm = normalize_title(&right_clean);

    if left_norm.is_empty() || right_norm.is_empty() {
        return false;
    }
    if left_norm == right_norm {
        return true;
    }

    // Direct containment when one is a substantial subset
    if (left_norm.contains(&right_norm) || right_norm.contains(&left_norm))
        && (left_norm.len() as f64 / right_norm.len() as f64).abs() >= 0.5
    {
        return true;
    }

    similarity(&left_clean, &right_clean) >= 0.80
}

/// Score duration difference:
/// - Full score (1.0) within 2.0s
/// - Linear decay down to 0.0 at 10.0s
/// - 0.0 for difference >= 10.0s
/// - Returns (score, is_known)
pub fn score_duration(track_dur: Option<f64>, cand_dur: Option<f64>) -> (f64, bool) {
    match (track_dur, cand_dur) {
        (Some(t), Some(c)) if t > 0.0 && c > 0.0 => {
            let diff = (t - c).abs();
            let score = if diff <= 2.0 {
                1.0
            } else if diff >= 10.0 {
                0.0
            } else {
                1.0 - (diff - 2.0) / 8.0
            };
            (score.clamp(0.0, 1.0), true)
        }
        _ => (1.0, false),
    }
}

/// Comprehensive candidate scoring matching track metadata against candidate metadata.
///
/// Features:
/// - Evaluates title, artist, album, and duration.
/// - Handles swapped Artist - Title order seamlessly.
/// - Evaluates duration full score within 2s, decaying to 0 at ~10s, only when both are known.
/// Minimum artist similarity for a candidate to be treated as the same recording.
///
/// Character-level metrics score two unrelated names surprisingly high — "Cold Hart"
/// against "Frank Carter & The Rattlesnakes" lands around 0.55 — so a strong title
/// match alone is not evidence that the lyrics belong to this track.
pub const ARTIST_MATCH_FLOOR: f64 = 0.62;

/// Tokens too common to prove anything about the artist ("the", "feat", …).
const ARTIST_STOPWORD_LEN: usize = 4;

/// Whether a candidate's artist plausibly names the same act as the track.
///
/// A missing artist on either side is treated as "cannot disprove" instead of "no
/// match", because several providers only fill the artist from the page title.
pub fn artist_matches(track_artist: &str, candidate_artist: &str) -> bool {
    let left = normalize_title(&clean_title(track_artist));
    let right = normalize_title(&clean_title(candidate_artist));
    if left.is_empty() || right.is_empty() {
        return true;
    }
    if left == right || left.contains(&right) || right.contains(&left) {
        return true;
    }
    let shares_token = |a: &str, b: &str| {
        a.split(' ')
            .filter(|token| token.len() >= ARTIST_STOPWORD_LEN)
            .any(|token| b.split(' ').any(|other| other == token))
    };
    if shares_token(&left, &right) || shares_token(&right, &left) {
        return true;
    }
    similarity(track_artist, candidate_artist) >= ARTIST_MATCH_FLOOR
}

pub fn score_candidate(track: &Track, cand: &Candidate) -> MatchScore {
    let direct_title = similarity(&track.title, &cand.title);
    let direct_artist = similarity(&track.artist, &cand.artist);

    // Check if Artist and Title are swapped:
    let swapped_title = similarity(&track.title, &cand.artist);
    let swapped_artist = similarity(&track.artist, &cand.title);

    let is_swapped = (swapped_title + swapped_artist) > (direct_title + direct_artist) + 0.20;

    let (title_score, artist_score) = if is_swapped {
        (swapped_title, swapped_artist)
    } else {
        (direct_title, direct_artist)
    };

    // Album scoring (only penalized when both album fields are populated and known)
    let (album_score, album_known) =
        match (track.album.trim(), cand.album.as_deref().map(str::trim)) {
            (t, Some(c)) if !t.is_empty() && !c.is_empty() => (similarity(t, c), true),
            _ => (1.0, false),
        };

    // Duration scoring
    let (duration_score, duration_known) =
        score_duration(track.duration_seconds, cand.duration_seconds);

    // Compute composite weighted total
    let total = match (duration_known, album_known) {
        (true, true) => {
            0.45 * title_score + 0.30 * artist_score + 0.10 * album_score + 0.15 * duration_score
        }
        (true, false) => 0.50 * title_score + 0.35 * artist_score + 0.15 * duration_score,
        (false, true) => 0.50 * title_score + 0.35 * artist_score + 0.15 * album_score,
        (false, false) => 0.60 * title_score + 0.40 * artist_score,
    };

    MatchScore {
        title: title_score,
        artist: artist_score,
        album: album_score,
        duration: duration_score,
        total: total.clamp(0.0, 1.0),
    }
}

// ============================================================================
// Similarity Metric Algorithms
// ============================================================================

/// Jaro-Winkler string distance metric with standard prefix scaling.
pub fn jaro_winkler(s1: &str, s2: &str) -> f64 {
    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();

    let len1 = s1_chars.len();
    let len2 = s2_chars.len();

    if len1 == 0 && len2 == 0 {
        return 1.0;
    }
    if len1 == 0 || len2 == 0 {
        return 0.0;
    }

    let match_distance = (len1.max(len2) / 2).saturating_sub(1);

    let mut s1_matches = vec![false; len1];
    let mut s2_matches = vec![false; len2];

    let mut matches = 0;
    for i in 0..len1 {
        let start = i.saturating_sub(match_distance);
        let end = (i + match_distance + 1).min(len2);

        for j in start..end {
            if s2_matches[j] || s1_chars[i] != s2_chars[j] {
                continue;
            }
            s1_matches[i] = true;
            s2_matches[j] = true;
            matches += 1;
            break;
        }
    }

    if matches == 0 {
        return 0.0;
    }

    let mut transpositions = 0;
    let mut k = 0;
    for i in 0..len1 {
        if !s1_matches[i] {
            continue;
        }
        while !s2_matches[k] {
            k += 1;
        }
        if s1_chars[i] != s2_chars[k] {
            transpositions += 1;
        }
        k += 1;
    }

    let m = matches as f64;
    let t = (transpositions as f64) / 2.0;
    let jaro = (m / len1 as f64 + m / len2 as f64 + (m - t) / m) / 3.0;

    // Winkler prefix adjustment (max 4 chars, prefix scale 0.1)
    let common_prefix = s1_chars
        .iter()
        .zip(s2_chars.iter())
        .take(4)
        .take_while(|(a, b)| a == b)
        .count() as f64;

    jaro + common_prefix * 0.1 * (1.0 - jaro)
}

/// Normalized Levenshtein similarity metric (1.0 = identical, 0.0 = completely disjoint).
pub fn normalized_levenshtein(s1: &str, s2: &str) -> f64 {
    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();

    let len1 = s1_chars.len();
    let len2 = s2_chars.len();

    if len1 == 0 && len2 == 0 {
        return 1.0;
    }
    if len1 == 0 || len2 == 0 {
        return 0.0;
    }

    let mut prev_row: Vec<usize> = (0..=len2).collect();
    let mut curr_row: Vec<usize> = vec![0; len2 + 1];

    for i in 1..=len1 {
        curr_row[0] = i;
        for j in 1..=len2 {
            let cost = if s1_chars[i - 1] == s2_chars[j - 1] {
                0
            } else {
                1
            };
            curr_row[j] = (curr_row[j - 1] + 1)
                .min(prev_row[j] + 1)
                .min(prev_row[j - 1] + cost);
        }
        prev_row.copy_from_slice(&curr_row);
    }

    let dist = prev_row[len2];
    let max_len = len1.max(len2);
    1.0 - (dist as f64 / max_len as f64)
}

/// Token-Set ratio: compares token sets (intersection and differences) to be robust against
/// reordered words, duplicate words, and subsets.
pub fn token_set_ratio(s1: &str, s2: &str) -> f64 {
    let tokens1: HashSet<&str> = s1.split_whitespace().collect();
    let tokens2: HashSet<&str> = s2.split_whitespace().collect();

    if tokens1.is_empty() && tokens2.is_empty() {
        return 1.0;
    }
    if tokens1.is_empty() || tokens2.is_empty() {
        return 0.0;
    }
    if tokens1 == tokens2 {
        return 1.0;
    }

    let mut intersect: Vec<&str> = tokens1.intersection(&tokens2).copied().collect();
    intersect.sort();

    let mut diff1_only: Vec<&str> = tokens1.difference(&tokens2).copied().collect();
    diff1_only.sort();

    let mut diff2_only: Vec<&str> = tokens2.difference(&tokens1).copied().collect();
    diff2_only.sort();

    let s_intersect = intersect.join(" ");

    let mut s1_comb = intersect.clone();
    s1_comb.extend_from_slice(&diff1_only);
    let s_comb1 = s1_comb.join(" ");

    let mut s2_comb = intersect.clone();
    s2_comb.extend_from_slice(&diff2_only);
    let s_comb2 = s2_comb.join(" ");

    let base_sim = |a: &str, b: &str| jaro_winkler(a, b).max(normalized_levenshtein(a, b));

    if !intersect.is_empty() {
        let sim_inter_comb1 = base_sim(&s_intersect, &s_comb1);
        let sim_inter_comb2 = base_sim(&s_intersect, &s_comb2);
        let sim_comb1_comb2 = base_sim(&s_comb1, &s_comb2);

        sim_inter_comb1.max(sim_inter_comb2).max(sim_comb1_comb2)
    } else {
        base_sim(&s_comb1, &s_comb2)
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LyricsStatus;

    #[test]
    fn test_cold_hart_noise_stripping() {
        // Exact titles sourced from the user's Cold Hart test directory:
        let test_cases = [
            ("Passion ft Scr1bz [Prod By Greaf]", "Passion"),
            ("Im Sorry [Prod By Artem]", "Im Sorry"),
            ("Anxiety [Prod By iLL instrumentals]", "Anxiety"),
            ("Heavy [Prod By Shawdi P]", "Heavy"),
            ("Really [prod By Greaf]", "Really"),
            ("On My Own ft. ColdSiemens [Prod By Natsu]", "On My Own"),
            ("Rock N Roll [Prod By L Beats]", "Rock N Roll"),
            ("Attitude [Prod by Pdub]", "Attitude"),
            ("Rockin Tommy [Prod By Lil Drill]", "Rockin Tommy"),
            (
                "With It Ft. Mackned & Wicca Phase Springs Eternal [Prod by Nedarb]",
                "With It",
            ),
            ("1. Cold Hart - 2 Am (Explicit)", "Cold Hart - 2 Am"),
            ("Morrissey () (Explicit)", "Morrissey"),
            ("Devil Baby (Explicit)", "Devil Baby"),
            (
                "IM TH3 N1GHT (Ft. Wicca Phase Springs Eternal)",
                "IM TH3 N1GHT",
            ),
            ("STR3SS3D (Ft. Yung Bruh)", "STR3SS3D"),
            ("L1K3 A R4V3R (Ft. Jpdreamthug)", "L1K3 A R4V3R"),
            ("CRO55 MY H4RT (Ft. Kool Nast)", "CRO55 MY H4RT"),
            ("JUST 17 FREESTYLE", "JUST 17 FREESTYLE"),
            ("M 3 N T H 0 L (Ft. Yung Bruh)", "M 3 N T H 0 L"),
            ("K1LL1N M3 (Ft. Gravestones)", "K1LL1N M3"),
            ("H3AV3N NOW (Ft. Wicca Phase Springs Eternal)", "H3AV3N NOW"),
            ("Memory (feat. Cold Hart) (2020)", "Memory"),
            ("Lovegang (feat. Cold Hart)", "Lovegang"),
            ("Bodybag (Explicit)", "Bodybag"),
            (
                "On The Run Ft, Lil Peep (Prod by LoudMouf Kang)",
                "On The Run",
            ),
            ("all of a sudden (prod lederrick)", "all of a sudden"),
            ("Black Jeep(Explicit)", "Black Jeep"),
            ("Song - Single Version", "Song"),
            ("Song (Remastered)", "Song"),
            ("Song (Official Video)", "Song"),
        ];

        for (input, expected) in test_cases {
            let cleaned = clean_title(input);
            assert_eq!(cleaned, expected, "Failed cleaning input: {input}");
        }
    }

    #[test]
    fn test_artist_gate_rejects_a_different_act_with_the_same_song_title() {
        // The real download of "Anxiety [Prod By iLL instrumentals]" by Cold Hart was
        // answered with Frank Carter & The Rattlesnakes' "Anxiety": the title matched
        // perfectly, the artist similarity was mid-range, and the composite score
        // cleared the 0.75 threshold. A title alone must not be enough.
        assert!(!artist_matches(
            "Cold Hart",
            "Frank Carter & The Rattlesnakes"
        ));

        // Same act, different spelling/casing/spacing.
        assert!(artist_matches("Cold Hart", "coldhart"));
        assert!(artist_matches("Cold Hart", "Cold Hart"));
        assert!(artist_matches("Cold Hart", "Cold Hart & Wicca Phase"));
        assert!(artist_matches("Beyoncé", "Beyonce"));
        assert!(artist_matches("Guns N' Roses", "Guns N Roses"));

        // Unknown artists cannot be disproved, so they are not rejected here.
        assert!(artist_matches("Cold Hart", ""));
        assert!(artist_matches("", "Anyone"));
    }

    #[test]
    fn search_title_strips_production_noise_but_keeps_a_usable_title() {
        let mut track = Track {
            audio_path: String::new(),
            lrc_path: String::new(),
            artist: "Cold Hart".to_string(),
            title: "Anxiety [Prod By iLL instrumentals]".to_string(),
            album: String::new(),
            duration_seconds: None,
            status: LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };
        assert_eq!(track.search_title(), "Anxiety");
        assert_eq!(track.search_artist(), "Cold Hart");

        // A title that is nothing but noise keeps its raw form rather than
        // searching for an empty string.
        track.title = "[Prod By Greaf]".to_string();
        assert_eq!(track.search_title(), "[Prod By Greaf]");
    }

    #[test]
    fn test_unicode_normalization() {
        // Latin with combining diacritics
        assert_eq!(normalize_title("Beyoncé"), "beyonce");
        assert_eq!(normalize_title("Motörhead"), "motorhead");
        assert_eq!(normalize_title("Rosalía"), "rosalia");
        assert_eq!(normalize_title("Dvořák"), "dvorak");

        // Cyrillic (Russian / Ukrainian / etc)
        assert_eq!(normalize_title("На заре"), "на заре");

        // CJK (Japanese / Chinese / Korean)
        assert_eq!(
            normalize_title("宇多田ヒカル - First Love"),
            "宇多田ヒカル first love"
        );

        // Devanagari (Hindi / Nepali)
        assert_eq!(normalize_title("नमस्ते"), "नमस्ते");
    }

    #[test]
    fn test_wrong_order_artist_title_swapped() {
        let track = Track {
            audio_path: String::new(),
            lrc_path: String::new(),
            artist: "Cold Hart".to_string(),
            title: "Passion".to_string(),
            album: String::new(),
            duration_seconds: Some(180.0),
            status: LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };

        // Candidate with Artist and Title reversed
        let cand = Candidate::new("Cold Hart", "Passion", None, Some(180.0));
        let score = score_candidate(&track, &cand);

        assert!(score.title >= 0.90, "Swapped title score: {}", score.title);
        assert!(
            score.artist >= 0.90,
            "Swapped artist score: {}",
            score.artist
        );
        assert!(
            score.is_match(),
            "Should match swapped candidate: {score:?}"
        );
    }

    #[test]
    fn test_extra_words_and_reordered_tokens() {
        let track = Track {
            audio_path: String::new(),
            lrc_path: String::new(),
            artist: "Cold Hart".to_string(),
            title: "Me and You".to_string(),
            album: String::new(),
            duration_seconds: Some(150.0),
            status: LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };

        // Candidate has featured artist and extra words
        let cand = Candidate::new(
            "Me and You (feat. Lil Peep) [Official Video]",
            "Cold Hart & Lil Peep",
            None,
            Some(151.0),
        );
        let score = score_candidate(&track, &cand);

        assert!(score.title >= 0.85, "Title score: {}", score.title);
        assert!(score.artist >= 0.70, "Artist score: {}", score.artist);
        assert!(score.duration == 1.0, "Duration within 2s should be 1.0");
        assert!(score.is_match(), "Score was: {score:?}");
    }

    #[test]
    fn test_non_latin_titles() {
        let track = Track {
            audio_path: String::new(),
            lrc_path: String::new(),
            artist: "宇多田ヒカル".to_string(),
            title: "First Love".to_string(),
            album: String::new(),
            duration_seconds: Some(250.0),
            status: LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };

        let cand = Candidate::new("First Love", "宇多田ヒカル", None, Some(250.5));
        let score = score_candidate(&track, &cand);

        assert!(score.title >= 0.95);
        assert!(score.artist >= 0.95);
        assert!(score.is_match());

        // Cyrillic
        assert!(titles_match("На заре (Remastered)", "На заре"));
        // Devanagari
        assert!(titles_match("नमस्ते सदा वत्सले", "नमस्ते सदा वत्सले"));
    }

    #[test]
    fn test_near_misses_that_must_not_match() {
        let track = Track {
            audio_path: String::new(),
            lrc_path: String::new(),
            artist: "Cold Hart".to_string(),
            title: "2 Am".to_string(),
            album: String::new(),
            duration_seconds: Some(120.0),
            status: LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };

        // 1. Near miss: "3 Am" is a different track
        let cand_diff_title = Candidate::new("3 Am", "Cold Hart", None, Some(120.0));
        let score_diff_title = score_candidate(&track, &cand_diff_title);
        assert!(
            !score_diff_title.is_match(),
            "Different number title should not match: {score_diff_title:?}"
        );

        // 2. Near miss: Elton John & Dua Lipa "Cold Heart" vs "Cold Hart"
        let cand_cold_heart =
            Candidate::new("Cold Heart", "Elton John, Dua Lipa", None, Some(200.0));
        let score_cold_heart = score_candidate(&track, &cand_cold_heart);
        assert!(
            !score_cold_heart.is_match(),
            "Cold Heart by Elton John should not match Cold Hart"
        );

        // 3. Near miss: Same title but different artist ("Hello" by Adele vs Lionel Richie)
        let adele_track = Track {
            audio_path: String::new(),
            lrc_path: String::new(),
            artist: "Adele".to_string(),
            title: "Hello".to_string(),
            album: String::new(),
            duration_seconds: Some(295.0),
            status: LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };
        let lionel_cand = Candidate::new("Hello", "Lionel Richie", None, Some(295.0));
        let score_lionel = score_candidate(&adele_track, &lionel_cand);
        assert!(
            !score_lionel.is_match(),
            "Different artist must not match: {score_lionel:?}"
        );

        // 4. Near miss: Significant duration discrepancy (e.g. > 10s difference)
        let cand_long = Candidate::new("2 Am", "Cold Hart", None, Some(240.0)); // 240s vs 120s
        let score_long = score_candidate(&track, &cand_long);
        assert_eq!(score_long.duration, 0.0);
        assert!(
            !score_long.is_match(),
            "Duration mismatch >= 10s must fail match"
        );
    }

    #[test]
    fn test_duration_decay() {
        // <= 2s is 1.0
        assert_eq!(score_duration(Some(100.0), Some(100.0)).0, 1.0);
        assert_eq!(score_duration(Some(100.0), Some(101.5)).0, 1.0);
        assert_eq!(score_duration(Some(100.0), Some(102.0)).0, 1.0);

        // At 6s difference: (6 - 2) / 8 = 0.5 -> 1.0 - 0.5 = 0.5
        let (score_6, known_6) = score_duration(Some(100.0), Some(106.0));
        assert!(known_6);
        assert!((score_6 - 0.5).abs() < 1e-6);

        // At >= 10s difference is 0.0
        assert_eq!(score_duration(Some(100.0), Some(110.0)).0, 0.0);
        assert_eq!(score_duration(Some(100.0), Some(130.0)).0, 0.0);

        // When one or both are None
        let (score_none, known_none) = score_duration(Some(100.0), None);
        assert!(!known_none);
        assert_eq!(score_none, 1.0);
    }
}
