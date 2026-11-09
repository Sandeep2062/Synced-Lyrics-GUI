use crate::fs::{write_atomic_with_backup, FileStoreError};
use crate::lrc::{audit_lrc, is_synced, parse_lines, parse_timestamps, LrcIssue, LrcLine};
use crate::matching::{score_candidate, similarity, Candidate};
use crate::model::{DownloadMode, Track};
use crate::providers::{LyricsProvider, ProviderError, ProviderName};
use crate::rate_limiter::{sleep_cancellable, AdaptiveRateLimiter, RateLimitConfig};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;
use thiserror::Error;

pub const DEFAULT_MATCH_THRESHOLD: f64 = 0.75;

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadEvent {
    Started {
        title: String,
    },
    Searching {
        provider: ProviderName,
    },
    ProviderFound {
        provider: ProviderName,
        synced: bool,
    },
    ProviderNotFound {
        provider: ProviderName,
    },
    Retrying {
        provider: ProviderName,
        attempt: u8,
        delay_seconds: u64,
    },
    ProviderFailed {
        provider: ProviderName,
        message: String,
    },
    Selected {
        provider: ProviderName,
        reason: String,
    },
    Written {
        provider: ProviderName,
        synced: bool,
    },
    Skipped,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadOutcome {
    Downloaded {
        provider: ProviderName,
        synced: bool,
    },
    Skipped,
    Cancelled,
    NotFound,
    DryRun {
        candidates: Vec<ScoredCandidate>,
        selected: Option<ProviderName>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredCandidate {
    pub candidate: Candidate,
    pub match_score: f64,
    pub is_synced: bool,
    pub passes_audit: bool,
    pub audit_issue: Option<LrcIssue>,
    pub duration_fit: f64,
    pub line_density: f64,
    pub rank_score: f64,
    pub timing_agreement_count: usize,
    pub selection_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub match_threshold: f64,
    pub dry_run: bool,
    pub max_threads: usize,
    pub backup_before_overwrite: bool,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            match_threshold: DEFAULT_MATCH_THRESHOLD,
            dry_run: false,
            max_threads: 4,
            backup_before_overwrite: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("could not write lyrics: {0}")]
    File(#[from] FileStoreError),
}

fn default_provider_limiter(name: ProviderName) -> &'static AdaptiveRateLimiter {
    static LIMITERS: OnceLock<HashMap<ProviderName, AdaptiveRateLimiter>> = OnceLock::new();
    let map = LIMITERS.get_or_init(|| {
        let mut m = HashMap::new();
        for &name in &ProviderName::ALL {
            let config = match name {
                ProviderName::Lrclib => RateLimitConfig {
                    requests_per_minute: 60,
                    requests_per_day: 0,
                    base_interval: Duration::from_millis(500),
                    max_backoff: Duration::from_secs(60),
                },
                ProviderName::Musixmatch => RateLimitConfig {
                    requests_per_minute: 40,
                    requests_per_day: 0,
                    base_interval: Duration::from_millis(1000),
                    max_backoff: Duration::from_secs(60),
                },
                ProviderName::Netease => RateLimitConfig {
                    requests_per_minute: 60,
                    requests_per_day: 0,
                    base_interval: Duration::from_millis(500),
                    max_backoff: Duration::from_secs(60),
                },
                ProviderName::Megalobiz => RateLimitConfig {
                    requests_per_minute: 30,
                    requests_per_day: 0,
                    base_interval: Duration::from_millis(1000),
                    max_backoff: Duration::from_secs(60),
                },
                ProviderName::Genius => RateLimitConfig {
                    requests_per_minute: 60,
                    requests_per_day: 0,
                    base_interval: Duration::from_millis(500),
                    max_backoff: Duration::from_secs(60),
                },
            };
            m.insert(name, AdaptiveRateLimiter::new(config));
        }
        m
    });
    map.get(&name).expect("all provider names initialized")
}

pub fn download_track(
    track: &Track,
    mode: DownloadMode,
    providers: &[Box<dyn LyricsProvider>],
    cancelled: &AtomicBool,
    emit: impl FnMut(DownloadEvent),
) -> Result<DownloadOutcome, DownloadError> {
    download_track_with_options(
        track,
        mode,
        providers,
        cancelled,
        DownloadOptions::default(),
        emit,
    )
}

pub fn download_track_dry_run(
    track: &Track,
    mode: DownloadMode,
    providers: &[Box<dyn LyricsProvider>],
    cancelled: &AtomicBool,
    match_threshold: Option<f64>,
) -> Result<Vec<ScoredCandidate>, DownloadError> {
    let options = DownloadOptions {
        match_threshold: match_threshold.unwrap_or(DEFAULT_MATCH_THRESHOLD),
        dry_run: true,
        max_threads: 4,
        backup_before_overwrite: true,
    };
    match download_track_with_options(track, mode, providers, cancelled, options, |_| {})? {
        DownloadOutcome::DryRun { candidates, .. } => Ok(candidates),
        _ => Ok(Vec::new()),
    }
}

pub fn download_track_with_options(
    track: &Track,
    mode: DownloadMode,
    providers: &[Box<dyn LyricsProvider>],
    cancelled: &AtomicBool,
    options: DownloadOptions,
    mut emit: impl FnMut(DownloadEvent),
) -> Result<DownloadOutcome, DownloadError> {
    emit(DownloadEvent::Started {
        title: track.display_title().to_string(),
    });

    if !track.should_download(mode) {
        emit(DownloadEvent::Skipped);
        return Ok(DownloadOutcome::Skipped);
    }

    if cancelled.load(Ordering::Relaxed) {
        emit(DownloadEvent::Cancelled);
        return Ok(DownloadOutcome::Cancelled);
    }

    if providers.is_empty() {
        return Ok(DownloadOutcome::NotFound);
    }

    // 1. Query providers in parallel with bounded thread pool and rate limiting
    let pool_size = options.max_threads.max(1).min(providers.len());
    let queue = Arc::new(Mutex::new(
        (0..providers.len()).collect::<VecDeque<usize>>(),
    ));
    let provider_results = Arc::new(Mutex::new(Vec::<(
        usize,
        Result<Vec<Candidate>, ProviderError>,
    )>::new()));
    let (tx, rx) = mpsc::channel();

    thread::scope(|s| {
        for _ in 0..pool_size {
            let tx = tx.clone();
            let queue = Arc::clone(&queue);
            let provider_results = Arc::clone(&provider_results);
            s.spawn(move || loop {
                if cancelled.load(Ordering::Relaxed) {
                    break;
                }
                let idx = {
                    let mut q = queue.lock().unwrap();
                    match q.pop_front() {
                        Some(i) => i,
                        None => break,
                    }
                };

                let provider = &providers[idx];
                let name = provider.name();
                let _ = tx.send(DownloadEvent::Searching { provider: name });

                let res = search_provider_with_retry_and_limiter(
                    provider.as_ref(),
                    track,
                    cancelled,
                    &tx,
                );

                match &res {
                    Ok(candidates) => {
                        let with_lyrics: Vec<&Candidate> = candidates
                            .iter()
                            .filter(|c| c.synced.is_some() || c.plain.is_some())
                            .collect();
                        if with_lyrics.is_empty() {
                            let _ = tx.send(DownloadEvent::ProviderNotFound { provider: name });
                        } else {
                            let has_synced = with_lyrics.iter().any(|c| c.is_synced());
                            let _ = tx.send(DownloadEvent::ProviderFound {
                                provider: name,
                                synced: has_synced,
                            });
                        }
                    }
                    Err(ProviderError::NotFound) => {
                        let _ = tx.send(DownloadEvent::ProviderNotFound { provider: name });
                    }
                    Err(err) => {
                        let _ = tx.send(DownloadEvent::ProviderFailed {
                            provider: name,
                            message: err.to_string(),
                        });
                    }
                }

                provider_results.lock().unwrap().push((idx, res));
            });
        }
        drop(tx);

        for event in rx {
            emit(event);
        }
    });

    if cancelled.load(Ordering::Relaxed) {
        emit(DownloadEvent::Cancelled);
        return Ok(DownloadOutcome::Cancelled);
    }

    // 2. Gather all candidates
    let mut raw_candidates = Vec::new();
    let results = provider_results.lock().unwrap();
    for (_idx, res) in results.iter() {
        if let Ok(cands) = res {
            for c in cands {
                if c.synced.is_some() || c.plain.is_some() {
                    raw_candidates.push(c.clone());
                }
            }
        }
    }

    if raw_candidates.is_empty() {
        return Ok(DownloadOutcome::NotFound);
    }

    // 3. Drop any candidates below match threshold
    let passing_candidates: Vec<Candidate> = raw_candidates
        .into_iter()
        .filter(|cand| {
            let score = score_candidate(track, cand);
            score.total >= options.match_threshold
        })
        .collect();

    if passing_candidates.is_empty() {
        return Ok(DownloadOutcome::NotFound);
    }

    // 4. Timing agreement and ranking
    let scored_candidates = evaluate_and_rank_candidates(track, passing_candidates);

    if scored_candidates.is_empty() {
        return Ok(DownloadOutcome::NotFound);
    }

    let best = scored_candidates.first().cloned().unwrap();

    // Emit selection explanation event
    if let Some(ref reason) = best.selection_reason {
        emit(DownloadEvent::Selected {
            provider: best.candidate.provider,
            reason: reason.clone(),
        });
    }

    // UpgradePlain mode: require synced lyrics
    if mode == DownloadMode::UpgradePlain && !best.is_synced {
        return Ok(DownloadOutcome::NotFound);
    }

    // SmartUpdate / FixSuspicious: never overwrite existing lyrics with lower score
    if mode == DownloadMode::SmartUpdate || mode == DownloadMode::FixSuspicious {
        if let Some(existing_score) = score_existing_lyrics(track) {
            if best.rank_score < existing_score {
                emit(DownloadEvent::Skipped);
                return Ok(DownloadOutcome::Skipped);
            }
        }
    }

    // Dry-run mode: return candidate scores without writing
    if options.dry_run {
        return Ok(DownloadOutcome::DryRun {
            candidates: scored_candidates,
            selected: Some(best.candidate.provider),
        });
    }

    // Write lyrics file
    let text = best.candidate.best_text().unwrap_or_default();
    write_atomic_with_backup(&track.lrc_path, text, options.backup_before_overwrite)?;
    let synced = best.is_synced;
    emit(DownloadEvent::Written {
        provider: best.candidate.provider,
        synced,
    });
    Ok(DownloadOutcome::Downloaded {
        provider: best.candidate.provider,
        synced,
    })
}

fn search_provider_with_retry_and_limiter(
    provider: &dyn LyricsProvider,
    track: &Track,
    cancelled: &AtomicBool,
    tx: &mpsc::Sender<DownloadEvent>,
) -> Result<Vec<Candidate>, ProviderError> {
    const MAX_RETRIES: u8 = 2;
    let mut attempt = 0;
    let limiter = provider
        .rate_limiter()
        .unwrap_or_else(|| default_provider_limiter(provider.name()));

    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }

        if !limiter.wait_cancellable(cancelled) {
            return Ok(Vec::new());
        }

        match provider.search(track) {
            Ok(candidates) => {
                limiter.relax();
                return Ok(candidates);
            }
            Err(error @ (ProviderError::Request(_) | ProviderError::RateLimited { .. }))
                if attempt < MAX_RETRIES =>
            {
                attempt += 1;
                if let ProviderError::RateLimited {
                    retry_after_seconds,
                } = error
                {
                    limiter.penalize(retry_after_seconds.map(Duration::from_secs));
                } else {
                    limiter.penalize(None);
                }

                let delay_seconds = match error {
                    ProviderError::RateLimited {
                        retry_after_seconds: Some(seconds),
                    } => seconds.min(60),
                    _ => 2_u64.pow(attempt.into()),
                };

                let _ = tx.send(DownloadEvent::Retrying {
                    provider: provider.name(),
                    attempt,
                    delay_seconds,
                });

                if sleep_cancellable(Duration::from_secs(delay_seconds), cancelled) {
                    return Ok(Vec::new());
                }
            }
            Err(error) => {
                if let ProviderError::RateLimited {
                    retry_after_seconds,
                } = error
                {
                    limiter.penalize(retry_after_seconds.map(Duration::from_secs));
                }
                return Err(error);
            }
        }
    }
}

pub fn normalize_lyric_line(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

pub fn align_and_compare_timing(lines_a: &[LrcLine], lines_b: &[LrcLine]) -> Option<f64> {
    let clean_a: Vec<(f64, String)> = lines_a
        .iter()
        .filter_map(|l| {
            let s = normalize_lyric_line(&l.text);
            if s.is_empty() {
                None
            } else {
                Some((l.timestamp_seconds, s))
            }
        })
        .collect();

    let clean_b: Vec<(f64, String)> = lines_b
        .iter()
        .filter_map(|l| {
            let s = normalize_lyric_line(&l.text);
            if s.is_empty() {
                None
            } else {
                Some((l.timestamp_seconds, s))
            }
        })
        .collect();

    if clean_a.len() < 3 || clean_b.len() < 3 {
        return None;
    }

    let mut matched_diffs = Vec::new();
    let mut cursor_b = 0;

    for (ts_a, text_a) in &clean_a {
        let search_window_end = (cursor_b + 12).min(clean_b.len());
        let mut found = None;
        for j in cursor_b..search_window_end {
            let (ts_b, text_b) = &clean_b[j];
            if text_a == text_b
                || (text_a.len() >= 4 && text_b.len() >= 4 && similarity(text_a, text_b) >= 0.85)
            {
                found = Some((j, (ts_a - ts_b).abs()));
                break;
            }
        }
        if let Some((matched_idx, diff)) = found {
            matched_diffs.push(diff);
            cursor_b = matched_idx + 1;
            if cursor_b >= clean_b.len() {
                break;
            }
        }
    }

    if matched_diffs.len() < 3 {
        return None;
    }

    matched_diffs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_diff = matched_diffs[matched_diffs.len() / 2];
    Some(median_diff)
}

fn compute_duration_fit(track_duration: Option<f64>, candidate: &Candidate) -> f64 {
    let cand_dur = candidate.duration_seconds.or_else(|| {
        candidate
            .synced
            .as_deref()
            .and_then(|text| parse_timestamps(text).last().copied())
    });
    match (track_duration, cand_dur) {
        (Some(t), Some(c)) if t > 0.0 && c > 0.0 => {
            let diff = (t - c).abs();
            (-diff / 12.0).exp().clamp(0.0, 1.0)
        }
        _ => 0.8,
    }
}

fn compute_line_density(line_count: usize, duration_seconds: Option<f64>) -> f64 {
    if line_count == 0 {
        return 0.0;
    }
    if line_count < 3 {
        return 0.2;
    }
    let dur_mins = duration_seconds.unwrap_or(200.0) / 60.0;
    let dur_mins = dur_mins.max(0.5);
    let lpm = line_count as f64 / dur_mins;
    if (8.0..=40.0).contains(&lpm) {
        1.0
    } else if lpm < 8.0 {
        (lpm / 8.0).clamp(0.2, 1.0)
    } else {
        (1.0 - (lpm - 40.0) / 40.0).clamp(0.5, 1.0)
    }
}

fn evaluate_and_rank_candidates(track: &Track, candidates: Vec<Candidate>) -> Vec<ScoredCandidate> {
    let mut parsed_synced_lines: HashMap<usize, Vec<LrcLine>> = HashMap::new();
    for (i, cand) in candidates.iter().enumerate() {
        if let Some(ref text) = cand.synced {
            let lines = parse_lines(text);
            if lines.len() >= 3 {
                parsed_synced_lines.insert(i, lines);
            }
        }
    }

    let mut agreements = vec![0usize; candidates.len()];
    let mut has_timing_disagreement = false;

    let synced_indices: Vec<usize> = parsed_synced_lines.keys().copied().collect();
    for i in 0..synced_indices.len() {
        for j in (i + 1)..synced_indices.len() {
            let idx_a = synced_indices[i];
            let idx_b = synced_indices[j];
            let lines_a = &parsed_synced_lines[&idx_a];
            let lines_b = &parsed_synced_lines[&idx_b];
            if let Some(diff) = align_and_compare_timing(lines_a, lines_b) {
                if diff <= 1.5 {
                    agreements[idx_a] += 1;
                    agreements[idx_b] += 1;
                } else {
                    has_timing_disagreement = true;
                }
            }
        }
    }

    let total_synced = synced_indices.len();
    let mut scored: Vec<ScoredCandidate> = candidates
        .into_iter()
        .enumerate()
        .map(|(i, cand)| {
            let match_score = score_candidate(track, &cand).total;
            let is_synced = cand.is_synced();
            let (passes_audit, audit_issue) = if let Some(ref text) = cand.synced {
                let report = audit_lrc(text, track.duration_seconds, &track.title, &[]);
                (report.is_valid(), report.issues.first().cloned())
            } else {
                (false, None)
            };

            let duration_fit = compute_duration_fit(track.duration_seconds, &cand);
            let line_count = cand
                .best_text()
                .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0);
            let line_density =
                compute_line_density(line_count, track.duration_seconds.or(cand.duration_seconds));

            let agree_count = agreements[i];
            let timing_majority_bonus = if has_timing_disagreement && total_synced > 1 {
                let ratio = agree_count as f64 / (total_synced - 1) as f64;
                ratio * 10.0
            } else {
                0.0
            };

            let synced_pts = if is_synced { 50.0 } else { 0.0 };
            let match_pts = match_score * 30.0;
            let audit_pts = if passes_audit { 10.0 } else { 0.0 };
            let dur_pts = duration_fit * 5.0;
            let density_pts = line_density * 5.0;

            let rank_score =
                synced_pts + match_pts + audit_pts + dur_pts + density_pts + timing_majority_bonus;

            ScoredCandidate {
                candidate: cand,
                match_score,
                is_synced,
                passes_audit,
                audit_issue,
                duration_fit,
                line_density,
                rank_score,
                timing_agreement_count: agree_count,
                selection_reason: None,
            }
        })
        .collect();

    // Sort descending by rank score
    scored.sort_by(|a, b| {
        b.rank_score
            .partial_cmp(&a.rank_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let any_plain = scored.iter().any(|c| !c.is_synced);
    let synced_count = scored.iter().filter(|c| c.is_synced).count();
    let any_audit_failure = scored.iter().any(|c| c.is_synced && !c.passes_audit);

    if let Some(best) = scored.first_mut() {
        let reason = if has_timing_disagreement
            && best.timing_agreement_count > 0
            && total_synced > 1
        {
            let agree_total = best.timing_agreement_count + 1;
            format!(
                "timing agreed with the majority of providers ({}/{} providers agreed)",
                agree_total, total_synced
            )
        } else if best.is_synced && any_plain && synced_count == 1 {
            "it provided synchronized lyrics while other providers only had plain lyrics"
                .to_string()
        } else if best.passes_audit && any_audit_failure {
            format!(
                "it had high match score ({:.2}) and passed LRC quality audit without timing issues",
                best.match_score
            )
        } else if best.is_synced {
            format!(
                "it had the highest composite match score ({:.2}) and valid synchronized lyrics",
                best.match_score
            )
        } else {
            format!(
                "it had the highest composite match score ({:.2})",
                best.match_score
            )
        };
        best.selection_reason = Some(format!(
            "Chose {} because {}",
            best.candidate.provider.label(),
            reason
        ));
    }

    scored
}

fn score_existing_lyrics(track: &Track) -> Option<f64> {
    let path = std::path::Path::new(&track.lrc_path);
    if !path.exists() {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let is_synced_lrc = is_synced(text);
    let (passes_audit, _audit_issue) = if is_synced_lrc {
        let report = audit_lrc(text, track.duration_seconds, &track.title, &[]);
        (report.is_valid(), report.issues.first().cloned())
    } else {
        (false, None)
    };

    let line_count = text.lines().filter(|l| !l.trim().is_empty()).count();
    let density = compute_line_density(line_count, track.duration_seconds);

    let duration_fit = if is_synced_lrc {
        if let Some(last_ts) = parse_timestamps(text).last().copied() {
            match track.duration_seconds {
                Some(td) if td > 0.0 => (-(td - last_ts).abs() / 12.0).exp().clamp(0.0, 1.0),
                _ => 0.8,
            }
        } else {
            0.5
        }
    } else {
        0.8
    };

    let match_score = 1.0;
    let synced_pts = if is_synced_lrc { 50.0 } else { 0.0 };
    let match_pts = match_score * 30.0;
    let audit_pts = if passes_audit { 10.0 } else { 0.0 };
    let dur_pts = duration_fit * 5.0;
    let dense_pts = density * 5.0;

    Some(synced_pts + match_pts + audit_pts + dur_pts + dense_pts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderName;
    use std::sync::atomic::AtomicBool;

    #[derive(Clone)]
    struct FakeProvider {
        provider: ProviderName,
        result: Result<Vec<Candidate>, ProviderError>,
    }

    impl LyricsProvider for FakeProvider {
        fn name(&self) -> ProviderName {
            self.provider
        }

        fn search(&self, _track: &Track) -> Result<Vec<Candidate>, ProviderError> {
            self.result.clone()
        }
    }

    struct FlakyProvider {
        attempts: std::sync::Mutex<u8>,
    }

    impl LyricsProvider for FlakyProvider {
        fn name(&self) -> ProviderName {
            ProviderName::Lrclib
        }

        fn search(&self, track: &Track) -> Result<Vec<Candidate>, ProviderError> {
            let mut attempts = self.attempts.lock().unwrap();
            *attempts += 1;
            if *attempts == 1 {
                Err(ProviderError::Request("temporary failure".to_string()))
            } else {
                Ok(vec![Candidate {
                    title: track.title.clone(),
                    artist: track.artist.clone(),
                    album: Some(track.album.clone()),
                    duration_seconds: track.duration_seconds,
                    synced: Some("[00:01]retried".to_string()),
                    plain: None,
                    provider: ProviderName::Lrclib,
                }])
            }
        }
    }

    fn track(path: &str) -> Track {
        Track {
            audio_path: format!("{path}.mp3"),
            lrc_path: format!("{path}.lrc"),
            artist: "Artist".to_string(),
            title: "Song".to_string(),
            album: "Album".to_string(),
            duration_seconds: Some(200.0),
            status: crate::model::LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        }
    }

    #[test]
    fn chooses_synced_result_and_writes_it() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-download-{}", std::process::id()));
        let target = track(root.to_str().unwrap());
        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Genius,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: None,
                    plain: Some("plain".to_string()),
                    provider: ProviderName::Genius,
                }]),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Lrclib,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: Some("[00:01]synced".to_string()),
                    plain: None,
                    provider: ProviderName::Lrclib,
                }]),
            }),
        ];
        let events = std::cell::RefCell::new(Vec::new());

        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |event| events.borrow_mut().push(event),
        )
        .unwrap();

        assert_eq!(
            outcome,
            DownloadOutcome::Downloaded {
                provider: ProviderName::Lrclib,
                synced: true,
            }
        );
        assert_eq!(
            std::fs::read_to_string(&target.lrc_path).unwrap(),
            "[00:01]synced"
        );
        assert!(events
            .borrow()
            .iter()
            .any(|event| matches!(event, DownloadEvent::Written { synced: true, .. })));
        assert!(events
            .borrow()
            .iter()
            .any(|event| matches!(event, DownloadEvent::Selected { .. })));
        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn retries_temporary_provider_failures() {
        let root = std::env::temp_dir().join(format!(
            "synced-lyrics-retry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = track(root.to_str().unwrap());
        let providers: Vec<Box<dyn LyricsProvider>> = vec![Box::new(FlakyProvider {
            attempts: std::sync::Mutex::new(0),
        })];
        let events = std::cell::RefCell::new(Vec::new());
        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |event| events.borrow_mut().push(event),
        )
        .unwrap();
        assert!(matches!(outcome, DownloadOutcome::Downloaded { .. }));
        assert!(events
            .borrow()
            .iter()
            .any(|event| matches!(event, DownloadEvent::Retrying { attempt: 1, .. })));
        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn upgrade_plain_ignores_plain_only_and_emits_provider_events() {
        let root = std::env::temp_dir().join(format!(
            "synced-lyrics-upgrade-plain-{}",
            std::process::id()
        ));
        let mut target = track(root.to_str().unwrap());
        target.status = crate::model::LyricsStatus::Plain;

        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Genius,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: None,
                    plain: Some("plain lyrics only".to_string()),
                    provider: ProviderName::Genius,
                }]),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Lrclib,
                result: Ok(Vec::new()),
            }),
        ];

        let events = std::cell::RefCell::new(Vec::new());
        let outcome = download_track(
            &target,
            DownloadMode::UpgradePlain,
            &providers,
            &AtomicBool::new(false),
            |event| events.borrow_mut().push(event),
        )
        .unwrap();

        assert_eq!(outcome, DownloadOutcome::NotFound);
        assert!(!std::path::Path::new(&target.lrc_path).exists());
        assert!(events.borrow().iter().any(|e| matches!(
            e,
            DownloadEvent::ProviderFound {
                provider: ProviderName::Genius,
                synced: false
            }
        )));
        assert!(events.borrow().iter().any(|e| matches!(
            e,
            DownloadEvent::ProviderNotFound {
                provider: ProviderName::Lrclib
            }
        )));
    }

    #[test]
    fn test_fake_provider_selects_highest_scoring_candidate_within_same_provider() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-rank-single-{}", std::process::id()));
        let target = track(root.to_str().unwrap());
        let providers: Vec<Box<dyn LyricsProvider>> = vec![Box::new(FakeProvider {
            provider: ProviderName::Lrclib,
            result: Ok(vec![
                Candidate {
                    title: "Song (Acoustic)".to_string(),
                    artist: "Artist".to_string(),
                    album: None,
                    duration_seconds: Some(150.0),
                    synced: Some("[00:01]acoustic".to_string()),
                    plain: None,
                    provider: ProviderName::Lrclib,
                },
                Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: Some("[00:01]exact match".to_string()),
                    plain: None,
                    provider: ProviderName::Lrclib,
                },
                Candidate {
                    title: "Completely Different Song".to_string(),
                    artist: "Different Artist".to_string(),
                    album: None,
                    duration_seconds: Some(300.0),
                    synced: Some("[00:01]wrong".to_string()),
                    plain: None,
                    provider: ProviderName::Lrclib,
                },
            ]),
        })];

        let events = std::cell::RefCell::new(Vec::new());
        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |event| events.borrow_mut().push(event),
        )
        .unwrap();

        assert_eq!(
            outcome,
            DownloadOutcome::Downloaded {
                provider: ProviderName::Lrclib,
                synced: true,
            }
        );
        assert_eq!(
            std::fs::read_to_string(&target.lrc_path).unwrap(),
            "[00:01]exact match"
        );
        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn test_fake_provider_prefers_synced_candidate_over_plain_candidate() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-synced-pref-{}", std::process::id()));
        let target = track(root.to_str().unwrap());
        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Genius,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: None,
                    plain: Some("plain text".to_string()),
                    provider: ProviderName::Genius,
                }]),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Netease,
                result: Ok(vec![Candidate {
                    title: "Song (Live)".to_string(),
                    artist: "Artist".to_string(),
                    album: None,
                    duration_seconds: Some(198.0),
                    synced: Some("[00:01]synced text".to_string()),
                    plain: None,
                    provider: ProviderName::Netease,
                }]),
            }),
        ];

        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        assert_eq!(
            outcome,
            DownloadOutcome::Downloaded {
                provider: ProviderName::Netease,
                synced: true,
            }
        );
        assert_eq!(
            std::fs::read_to_string(&target.lrc_path).unwrap(),
            "[00:01]synced text"
        );
        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn test_fake_provider_compares_multiple_synced_candidates_by_confidence() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-synced-comp-{}", std::process::id()));
        let target = track(root.to_str().unwrap());
        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Megalobiz,
                result: Ok(vec![Candidate {
                    title: "Song Remix".to_string(),
                    artist: "Artist".to_string(),
                    album: None,
                    duration_seconds: Some(240.0),
                    synced: Some("[00:01]remix".to_string()),
                    plain: None,
                    provider: ProviderName::Megalobiz,
                }]),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Musixmatch,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: Some("[00:01]best match".to_string()),
                    plain: None,
                    provider: ProviderName::Musixmatch,
                }]),
            }),
        ];

        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        assert_eq!(
            outcome,
            DownloadOutcome::Downloaded {
                provider: ProviderName::Musixmatch,
                synced: true,
            }
        );
        assert_eq!(
            std::fs::read_to_string(&target.lrc_path).unwrap(),
            "[00:01]best match"
        );
        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn test_fake_provider_not_found_when_empty_or_no_lyrics() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-notfound-{}", std::process::id()));
        let target = track(root.to_str().unwrap());
        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Lrclib,
                result: Ok(Vec::new()),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Genius,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: None,
                    duration_seconds: None,
                    synced: None,
                    plain: None,
                    provider: ProviderName::Genius,
                }]),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Netease,
                result: Err(ProviderError::NotFound),
            }),
        ];

        let events = std::cell::RefCell::new(Vec::new());
        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |e| events.borrow_mut().push(e),
        )
        .unwrap();

        assert_eq!(outcome, DownloadOutcome::NotFound);
        assert!(!std::path::Path::new(&target.lrc_path).exists());
        let not_found_count = events
            .borrow()
            .iter()
            .filter(|e| matches!(e, DownloadEvent::ProviderNotFound { .. }))
            .count();
        assert_eq!(not_found_count, 3);
    }

    #[test]
    fn test_wrong_song_high_confidence_result_must_lose() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-wrong-song-{}", std::process::id()));
        let target = track(root.to_str().unwrap());

        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Musixmatch,
                result: Ok(vec![Candidate {
                    title: "Completely Unrelated Track".to_string(),
                    artist: "Random Unrelated Artist".to_string(),
                    album: Some("Unrelated Album".to_string()),
                    duration_seconds: Some(300.0),
                    synced: Some("[00:10.00]wrong song lyrics line 1\n[00:15.00]wrong song lyrics line 2\n[00:20.00]wrong song lyrics line 3".to_string()),
                    plain: None,
                    provider: ProviderName::Musixmatch,
                }]),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Genius,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: None,
                    plain: Some("correct song plain lyrics".to_string()),
                    provider: ProviderName::Genius,
                }]),
            }),
        ];

        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        // The wrong-song result (even if synced) must be dropped because its match score is < 0.75.
        // The plain lyrics for the correct song must win!
        assert_eq!(
            outcome,
            DownloadOutcome::Downloaded {
                provider: ProviderName::Genius,
                synced: false,
            }
        );
        assert_eq!(
            std::fs::read_to_string(&target.lrc_path).unwrap(),
            "correct song plain lyrics"
        );
        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn test_plain_only_vs_synced() {
        let root = std::env::temp_dir().join(format!(
            "synced-lyrics-plain-vs-synced-{}",
            std::process::id()
        ));
        let target = track(root.to_str().unwrap());

        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Genius,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: None,
                    plain: Some("plain only lyrics".to_string()),
                    provider: ProviderName::Genius,
                }]),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Lrclib,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: Some(
                        "[00:10.00]synced line 1\n[00:15.00]synced line 2\n[00:20.00]synced line 3"
                            .to_string(),
                    ),
                    plain: None,
                    provider: ProviderName::Lrclib,
                }]),
            }),
        ];

        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        // Synced must always beat plain for matching songs
        assert_eq!(
            outcome,
            DownloadOutcome::Downloaded {
                provider: ProviderName::Lrclib,
                synced: true,
            }
        );
        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn test_provider_failure_resilience() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-prov-fail-{}", std::process::id()));
        let target = track(root.to_str().unwrap());

        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Megalobiz,
                result: Err(ProviderError::Request("Connection timed out".to_string())),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Lrclib,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: Some("[00:01]lyrics from working provider".to_string()),
                    plain: None,
                    provider: ProviderName::Lrclib,
                }]),
            }),
        ];

        let events = std::cell::RefCell::new(Vec::new());
        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |e| events.borrow_mut().push(e),
        )
        .unwrap();

        assert_eq!(
            outcome,
            DownloadOutcome::Downloaded {
                provider: ProviderName::Lrclib,
                synced: true,
            }
        );
        assert!(events.borrow().iter().any(|e| matches!(
            e,
            DownloadEvent::ProviderFailed {
                provider: ProviderName::Megalobiz,
                ..
            }
        )));
        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn test_cancellation_aborts_promptly() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-cancellation-{}", std::process::id()));
        let target = track(root.to_str().unwrap());

        let providers: Vec<Box<dyn LyricsProvider>> = vec![Box::new(FakeProvider {
            provider: ProviderName::Lrclib,
            result: Ok(vec![Candidate {
                title: "Song".to_string(),
                artist: "Artist".to_string(),
                album: Some("Album".to_string()),
                duration_seconds: Some(200.0),
                synced: Some("[00:01]cancelled".to_string()),
                plain: None,
                provider: ProviderName::Lrclib,
            }]),
        })];

        let cancelled = AtomicBool::new(true);
        let events = std::cell::RefCell::new(Vec::new());
        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &cancelled,
            |e| events.borrow_mut().push(e),
        )
        .unwrap();

        assert_eq!(outcome, DownloadOutcome::Cancelled);
        assert!(!std::path::Path::new(&target.lrc_path).exists());
        assert!(events
            .borrow()
            .iter()
            .any(|e| matches!(e, DownloadEvent::Cancelled)));
    }

    #[test]
    fn test_majority_timing_agreement_prefers_majority() {
        let root = std::env::temp_dir().join(format!(
            "synced-lyrics-majority-timing-{}",
            std::process::id()
        ));
        let target = track(root.to_str().unwrap());

        // 3 providers:
        // Lrclib has timing: 10.0, 15.0, 20.0
        // Netease has timing: 10.1, 15.1, 20.1 (Agrees with Lrclib)
        // Megalobiz has timing: 14.0, 19.0, 24.0 (Disagrees by ~4s)
        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Megalobiz,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: Some("[00:14.00]Line one text\n[00:19.00]Line two text\n[00:24.00]Line three text".to_string()),
                    plain: None,
                    provider: ProviderName::Megalobiz,
                }]),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Lrclib,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: Some("[00:10.00]Line one text\n[00:15.00]Line two text\n[00:20.00]Line three text".to_string()),
                    plain: None,
                    provider: ProviderName::Lrclib,
                }]),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Netease,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: Some("[00:10.10]Line one text\n[00:15.10]Line two text\n[00:20.10]Line three text".to_string()),
                    plain: None,
                    provider: ProviderName::Netease,
                }]),
            }),
        ];

        let events = std::cell::RefCell::new(Vec::new());
        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |e| events.borrow_mut().push(e),
        )
        .unwrap();

        // Either Lrclib or Netease must be chosen (they agreed with each other / majority)
        // Megalobiz must lose!
        match outcome {
            DownloadOutcome::Downloaded { provider, synced } => {
                assert!(synced);
                assert!(provider == ProviderName::Lrclib || provider == ProviderName::Netease);
            }
            other => panic!("expected Downloaded, got {:?}", other),
        }

        let selected_event = events
            .borrow()
            .iter()
            .find(|e| matches!(e, DownloadEvent::Selected { .. }))
            .cloned();
        assert!(selected_event.is_some());
        if let Some(DownloadEvent::Selected { provider, reason }) = selected_event {
            assert!(provider == ProviderName::Lrclib || provider == ProviderName::Netease);
            assert!(reason.contains("majority"));
        }

        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn test_smart_update_does_not_overwrite_with_lower_score() {
        let root = std::env::temp_dir().join(format!(
            "synced-lyrics-smart-update-protect-{}",
            std::process::id()
        ));
        let mut target = track(root.to_str().unwrap());
        target.status = crate::model::LyricsStatus::Synced;

        // Write existing high-quality synced lyrics
        let existing_lyrics = "[00:10.00]Line 1\n[00:15.00]Line 2\n[00:20.00]Line 3\n[00:25.00]Line 4\n[00:30.00]Line 5";
        std::fs::write(&target.lrc_path, existing_lyrics).unwrap();

        // Provider only returns plain lyrics (lower score)
        let providers: Vec<Box<dyn LyricsProvider>> = vec![Box::new(FakeProvider {
            provider: ProviderName::Genius,
            result: Ok(vec![Candidate {
                title: "Song".to_string(),
                artist: "Artist".to_string(),
                album: Some("Album".to_string()),
                duration_seconds: Some(200.0),
                synced: None,
                plain: Some("plain lyrics only".to_string()),
                provider: ProviderName::Genius,
            }]),
        })];

        let outcome = download_track(
            &target,
            DownloadMode::SmartUpdate,
            &providers,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        // Must NOT overwrite!
        assert_eq!(outcome, DownloadOutcome::Skipped);
        assert_eq!(
            std::fs::read_to_string(&target.lrc_path).unwrap(),
            existing_lyrics
        );

        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn test_dry_run_mode_returns_candidates_and_scores_without_writing() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-dry-run-{}", std::process::id()));
        let target = track(root.to_str().unwrap());

        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Lrclib,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: Some("[00:10.00]synced line".to_string()),
                    plain: None,
                    provider: ProviderName::Lrclib,
                }]),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Genius,
                result: Ok(vec![Candidate {
                    title: "Song".to_string(),
                    artist: "Artist".to_string(),
                    album: Some("Album".to_string()),
                    duration_seconds: Some(200.0),
                    synced: None,
                    plain: Some("plain line".to_string()),
                    provider: ProviderName::Genius,
                }]),
            }),
        ];

        let candidates = download_track_dry_run(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            Some(0.75),
        )
        .unwrap();

        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].rank_score >= candidates[1].rank_score);
        assert!(candidates[0].is_synced);
        assert_eq!(candidates[0].candidate.provider, ProviderName::Lrclib);

        // Crucial: file must NOT have been written
        assert!(!std::path::Path::new(&target.lrc_path).exists());
    }

    #[test]
    fn test_download_track_creates_backup_when_overwriting() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-dl-bak-{}", std::process::id()));
        let target = track(root.to_str().unwrap());
        let lrc_path = std::path::PathBuf::from(&target.lrc_path);
        let bak_path = std::path::PathBuf::from(format!("{}.bak", target.lrc_path));

        // Pre-create existing lrc file
        std::fs::create_dir_all(lrc_path.parent().unwrap()).unwrap();
        std::fs::write(&lrc_path, "[00:01.00]original lyrics").unwrap();

        let providers: Vec<Box<dyn LyricsProvider>> = vec![Box::new(FakeProvider {
            provider: ProviderName::Lrclib,
            result: Ok(vec![Candidate {
                title: "Song".to_string(),
                artist: "Artist".to_string(),
                album: Some("Album".to_string()),
                duration_seconds: Some(200.0),
                synced: Some("[00:10.00]new downloaded synced lyrics".to_string()),
                plain: None,
                provider: ProviderName::Lrclib,
            }]),
        })];

        let mut opts = DownloadOptions::default();
        opts.backup_before_overwrite = true;

        let outcome = download_track_with_options(
            &target,
            DownloadMode::ReplaceAll,
            &providers,
            &AtomicBool::new(false),
            opts,
            |_| {},
        )
        .unwrap();

        assert!(matches!(outcome, DownloadOutcome::Downloaded { .. }));
        assert!(bak_path.is_file());
        assert_eq!(
            std::fs::read_to_string(&bak_path).unwrap(),
            "[00:01.00]original lyrics"
        );
        assert_eq!(
            std::fs::read_to_string(&lrc_path).unwrap(),
            "[00:10.00]new downloaded synced lyrics"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
