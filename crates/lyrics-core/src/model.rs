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
}

impl Track {
    pub fn display_title(&self) -> &str {
        &self.title
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
