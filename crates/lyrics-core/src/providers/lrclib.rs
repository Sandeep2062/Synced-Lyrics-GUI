use super::{LyricsProvider, ProviderError, ProviderLyrics, ProviderName};
use crate::model::Track;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

pub struct LrclibProvider {
    client: Client,
    api_url: String,
}

impl LrclibProvider {
    pub fn new(base_url: &str) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("SyncedLyricsGUI/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        Ok(Self {
            client,
            api_url: format!("{}/api/get", base_url.trim_end_matches('/')),
        })
    }

    pub fn from_client(client: Client, api_url: impl Into<String>) -> Self {
        Self {
            client,
            api_url: api_url.into(),
        }
    }
}

impl LyricsProvider for LrclibProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Lrclib
    }

    fn search(&self, track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
        let response = self
            .client
            .get(&self.api_url)
            .query(&[
                ("track_name", track.title.as_str()),
                ("artist_name", track.artist.as_str()),
                ("album_name", track.album.as_str()),
            ])
            .send()
            .map_err(|error| ProviderError::Request(error.to_string()))?;

        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse().ok()),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "LRCLib returned HTTP {}",
                response.status()
            )));
        }

        let payload = response
            .json::<LrclibResponse>()
            .map_err(|error| ProviderError::Parse(error.to_string()))?;
        parse_response(payload).map(Some)
    }
}

#[derive(Debug, Deserialize)]
struct LrclibResponse {
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
    instrumental: Option<bool>,
}

fn parse_response(payload: LrclibResponse) -> Result<ProviderLyrics, ProviderError> {
    let synced = non_empty(payload.synced_lyrics);
    let plain = non_empty(payload.plain_lyrics);
    if synced.is_none() && plain.is_none() && !payload.instrumental.unwrap_or(false) {
        return Err(ProviderError::NotFound);
    }
    let confidence = if payload.instrumental.unwrap_or(false) {
        0.1
    } else if synced.is_some() {
        0.95
    } else {
        0.7
    };
    Ok(ProviderLyrics {
        provider: ProviderName::Lrclib,
        synced,
        plain,
        confidence,
    })
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synced_and_plain_fields() {
        let payload: LrclibResponse = serde_json::from_str(
            r#"{"syncedLyrics":"[00:01]hello","plainLyrics":"hello","instrumental":false}"#,
        )
        .unwrap();
        let lyrics = parse_response(payload).unwrap();
        assert_eq!(lyrics.provider, ProviderName::Lrclib);
        assert_eq!(lyrics.synced.as_deref(), Some("[00:01]hello"));
        assert_eq!(lyrics.plain.as_deref(), Some("hello"));
    }

    #[test]
    fn treats_instrumental_response_as_empty_result() {
        let payload: LrclibResponse =
            serde_json::from_str(r#"{"syncedLyrics":null,"plainLyrics":null,"instrumental":true}"#)
                .unwrap();
        let lyrics = parse_response(payload).unwrap();
        assert!(lyrics.best_text().is_none());
    }
}
