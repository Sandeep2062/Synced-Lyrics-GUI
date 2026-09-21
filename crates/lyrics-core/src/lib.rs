pub mod art;
pub mod audio;
pub mod config;
pub mod db;
pub mod download;
pub mod fs;
pub mod lrc;
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
pub use db::{LibraryDb, RepositoryError};
pub use download::{download_track, DownloadEvent, DownloadOutcome};
pub use fs::{write_atomic, FileStoreError};
pub use lrc::{
    audit_lrc, is_synced, parse_lines, parse_timestamps, titles_match, LrcIssue, LrcLine,
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
