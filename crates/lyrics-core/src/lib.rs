pub mod art;
pub mod audio;
pub mod config;
pub mod db;
pub mod diff;
pub mod download;
pub mod fs;
pub mod health;
pub mod identify;
pub mod lrc;
pub mod matching;
pub mod model;
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
