use crate::model::Track;
pub mod genius;
pub mod lrclib;
pub mod megalobiz;
pub mod musixmatch;
pub mod netease;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderName {
    Lrclib,
    Musixmatch,
    Netease,
    Megalobiz,
    Genius,
}

impl ProviderName {
    pub const ALL: [Self; 5] = [
        Self::Lrclib,
        Self::Musixmatch,
        Self::Netease,
        Self::Megalobiz,
        Self::Genius,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Lrclib => "LRCLib",
            Self::Musixmatch => "Musixmatch",
            Self::Netease => "NetEase",
            Self::Megalobiz => "Megalobiz",
            Self::Genius => "Genius",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderLyrics {
    pub provider: ProviderName,
    pub synced: Option<String>,
    pub plain: Option<String>,
    pub confidence: f32,
}

impl ProviderLyrics {
    pub fn best_text(&self) -> Option<&str> {
        self.synced.as_deref().or(self.plain.as_deref())
    }

    pub fn is_synced(&self) -> bool {
        self.synced.is_some()
    }
}

#[derive(Debug, Clone, Error)]
pub enum ProviderError {
    #[error("provider request failed: {0}")]
    Request(String),
    #[error("provider response could not be parsed: {0}")]
    Parse(String),
    #[error("provider rate limit reached; retry after {retry_after_seconds:?} seconds")]
    RateLimited { retry_after_seconds: Option<u64> },
    #[error("provider has no matching lyrics")]
    NotFound,
}

pub trait LyricsProvider: Send + Sync {
    fn name(&self) -> ProviderName;
    fn search(&self, track: &Track) -> Result<Option<ProviderLyrics>, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_synchronized_provider_text() {
        let result = ProviderLyrics {
            provider: ProviderName::Lrclib,
            synced: Some("[00:01]synced".to_string()),
            plain: Some("plain".to_string()),
            confidence: 0.9,
        };
        assert_eq!(result.best_text(), Some("[00:01]synced"));
        assert!(result.is_synced());
    }
}
