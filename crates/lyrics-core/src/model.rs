use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LyricsStatus {
    Missing,
    Plain,
    Synced,
    Suspicious,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadMode {
    MissingOnly,
    SmartUpdate,
    UpgradePlain,
    FixSuspicious,
    ReplaceAll,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub audio_path: String,
    pub lrc_path: String,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub duration_seconds: Option<f64>,
    pub status: LyricsStatus,
    pub mtime: Option<f64>,
    pub last_checked: Option<f64>,
    pub suspicious_reason: Option<String>,
    #[serde(default)]
    pub lyrics_offset_ms: i64,
}

impl Track {
    pub fn display_title(&self) -> &str {
        &self.title
    }

    /// Title as it should be sent to a search engine.
    ///
    /// Library filenames and tags carry production notes and features
    /// ("Anxiety [Prod By iLL instrumentals]", "Song (Remastered)"). Those words
    /// actively poison provider search — Genius returns no song hit for the raw
    /// title but finds the track for "Anxiety" — so queries use the cleaned form
    /// and fall back to the raw title when cleaning removes everything.
    pub fn search_title(&self) -> String {
        let cleaned = crate::matching::clean_title(&self.title);
        if cleaned.trim().is_empty() {
            self.title.clone()
        } else {
            cleaned
        }
    }

    /// Artist as it should be sent to a search engine (same reasoning as
    /// [`Track::search_title`]).
    pub fn search_artist(&self) -> String {
        let cleaned = crate::matching::clean_title(&self.artist);
        if cleaned.trim().is_empty() {
            self.artist.clone()
        } else {
            cleaned
        }
    }

    pub fn should_download(&self, mode: DownloadMode) -> bool {
        match mode {
            DownloadMode::MissingOnly => self.status == LyricsStatus::Missing,
            DownloadMode::SmartUpdate => matches!(
                self.status,
                LyricsStatus::Missing | LyricsStatus::Plain | LyricsStatus::Suspicious
            ),
            DownloadMode::UpgradePlain => self.status == LyricsStatus::Plain,
            DownloadMode::FixSuspicious => self.status == LyricsStatus::Suspicious,
            DownloadMode::ReplaceAll => true,
        }
    }
}
