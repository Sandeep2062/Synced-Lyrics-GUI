pub mod art;
pub mod audio;
pub mod config;
pub mod db;
pub mod diff;
pub mod download;
pub mod fs;
pub mod golden;
pub mod health;
pub mod identify;
pub mod lrc;
pub mod matching;
pub mod model;
pub mod panic_hook;
pub mod providers;
pub mod rate_limiter;
pub mod scanner;

pub use art::{
    artwork_stamp, clear_folder_art_cache, extract_cover_art, extract_cover_art_with_fallback,
    folder_art_fingerprint, folder_cover_art, folder_cover_thumbnail, thumbnail_jpeg, ArtCache,
    ThumbnailStore, THUMBNAIL_MAX_PX,
};
pub use audio::{AudioEngine, WaveformStore};
pub use config::{app_data_dir, load_settings, save_settings, AppSettings, ConfigError};
pub use db::{BatchJobItemDraft, BatchJobItemRecord, BatchJobRecord, LibraryDb, RepositoryError};
pub use diff::{compute_line_diff, generate_unified_diff, DiffLine, DiffLineType};
pub use download::{
    download_track, download_track_dry_run, download_track_with_options, DownloadError,
    DownloadEvent, DownloadOptions, DownloadOutcome, ScoredCandidate,
};
pub use fs::{backup_lyrics_file, write_atomic, write_atomic_with_backup, FileStoreError};
pub use golden::{
    build_golden_set, evaluate_golden_set, format_evaluation_report, GoldenCategory,
    GoldenEvaluationReport, GoldenThresholds, GoldenTrack, MetricScore,
};
pub use health::{
    apply_offset_to_lrc, categorize_track_issue, compute_health_summary, parse_offset_from_reason,
    HealthCategory, HealthIssue, HealthStats,
};
pub use identify::{
    apply_tag_suggestion, identify_track, identify_via_lrclib_search, match_filename_against_tags,
    parse_filename, rank_lrclib_candidates, track_needs_identification, FilenamePattern,
    IdentificationSuggestion, ParsedFilename, TagWriteError,
};
pub use lrc::{
    apply_offset_tag, audit_lrc, find_active_line_index, find_active_word_index, is_synced,
    parse_lines, parse_metadata, parse_timestamps, rewrite_timestamps_with_offset, titles_match,
    LrcAuditReport, LrcIssue, LrcLine, LrcMetadata, LrcWord,
};
pub use matching::{
    clean_title, normalize_title, score_candidate, score_duration, similarity, Candidate,
    MatchScore,
};
pub use model::{DownloadMode, LyricsStatus, Track};
pub use panic_hook::{log_panic_to_dir, setup_panic_hook};
pub use providers::genius::GeniusProvider;
pub use providers::lrclib::LrclibProvider;
pub use providers::megalobiz::MegalobizProvider;
pub use providers::musixmatch::MusixmatchProvider;
pub use providers::netease::NeteaseProvider;
pub use providers::{LyricsProvider, ProviderError, ProviderLyrics, ProviderName};
pub use rate_limiter::{AdaptiveRateLimiter, RateLimitConfig};
pub use scanner::{
    collect_audio_paths, scan_audio_paths_incremental, scan_directory, scan_directory_incremental,
    scan_directory_incremental_with_progress, IncrementalScanResult, ScanError, ScanSummary,
};

/// Helper trait to acquire a `Mutex` guard, recovering the data even if the mutex
/// was poisoned by a panicked thread.
pub trait MutexExt<T> {
    fn lock_ignore_poison(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for std::sync::Mutex<T> {
    fn lock_ignore_poison(&self) -> std::sync::MutexGuard<'_, T> {
        match self.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Standalone helper function to lock a mutex and recover if poisoned.
pub fn lock_ignore_poison<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn test_mutex_poison_recovery() {
        let lock = Arc::new(Mutex::new(42));
        let lock_clone = Arc::clone(&lock);

        // Spawn a thread that acquires the lock and panics, poisoning the mutex
        let handle = thread::spawn(move || {
            let mut guard = lock_clone.lock().unwrap();
            *guard = 99;
            panic!("deliberate panic to poison mutex");
        });
        assert!(handle.join().is_err(), "Worker thread must have panicked");

        // Calling standard lock().is_err() verifies the mutex is indeed poisoned
        assert!(lock.lock().is_err(), "Mutex must be poisoned");

        // Trait method lock_ignore_poison recovers smoothly
        {
            let mut guard = lock.lock_ignore_poison();
            assert_eq!(*guard, 99);
            *guard = 100;
        }

        // Standalone helper function also recovers smoothly
        {
            let guard = lock_ignore_poison(&lock);
            assert_eq!(*guard, 100);
        }
    }
}
