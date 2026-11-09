use super::{LyricsProvider, ProviderError, ProviderName};
use crate::matching::Candidate;
use crate::model::Track;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

pub struct LrclibProvider {
    client: Client,
    get_url: String,
    search_url: String,
}

impl LrclibProvider {
    pub fn new(base_url: &str) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("SyncedLyricsGUI/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        let base = base_url
            .trim_end_matches('/')
            .trim_end_matches("/api/get")
            .trim_end_matches("/api/search");
        Ok(Self {
            client,
            get_url: format!("{}/api/get", base),
            search_url: format!("{}/api/search", base),
        })
    }

    pub fn from_client(client: Client, base_url: impl Into<String>) -> Self {
        let base_str = base_url.into();
        let base = base_str
            .trim_end_matches('/')
            .trim_end_matches("/api/get")
            .trim_end_matches("/api/search");
        Self {
            client,
            get_url: format!("{}/api/get", base),
            search_url: format!("{}/api/search", base),
        }
    }

    pub fn with_urls(
        client: Client,
        get_url: impl Into<String>,
        search_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            get_url: get_url.into(),
            search_url: search_url.into(),
        }
    }
}

impl LyricsProvider for LrclibProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Lrclib
    }

    fn search(&self, track: &Track) -> Result<Vec<Candidate>, ProviderError> {
        // Step 1: Try /api/get with duration first
        let mut get_params = vec![
            ("track_name", track.title.as_str()),
            ("artist_name", track.artist.as_str()),
        ];
        let duration_str;
        if let Some(dur) = track.duration_seconds {
            duration_str = format!("{:.0}", dur);
            get_params.push(("duration", duration_str.as_str()));
        }
        if !track.album.trim().is_empty() {
            get_params.push(("album_name", track.album.as_str()));
        }

        let get_response = self.client.get(&self.get_url).query(&get_params).send();

        match get_response {
            Ok(response) => {
                if response.status().as_u16() == 429 {
                    return Err(ProviderError::RateLimited {
                        retry_after_seconds: parse_retry_after(response.headers()),
                    });
                }
                if response.status().is_success() {
                    if let Ok(record) = response.json::<LrclibRecord>() {
                        if let Some(candidate) = parse_record(record, track) {
                            return Ok(vec![candidate]);
                        }
                    }
                }
                // If 404 or no lyrics, proceed to search fallback
            }
            Err(_) => {
                // If network/request error on get, attempt search fallback
            }
        }

        // Step 2: Fall back to /api/search (track_name+artist_name or q)
        let mut candidates = self.execute_search(track, false)?;
        if candidates.is_empty() {
            // If track_name+artist_name returned nothing, try fuzzy q search
            candidates = self.execute_search(track, true)?;
        }

        Ok(candidates)
    }
}

impl LrclibProvider {
    fn execute_search(&self, track: &Track, use_q: bool) -> Result<Vec<Candidate>, ProviderError> {
        let req = if use_q || track.title.trim().is_empty() || track.artist.trim().is_empty() {
            let q = format!("{} {}", track.title, track.artist);
            self.client.get(&self.search_url).query(&[("q", q.trim())])
        } else {
            let mut search_params = vec![
                ("track_name", track.title.as_str()),
                ("artist_name", track.artist.as_str()),
            ];
            if !track.album.trim().is_empty() {
                search_params.push(("album_name", track.album.as_str()));
            }
            self.client.get(&self.search_url).query(&search_params)
        };

        let response = req
            .send()
            .map_err(|error| ProviderError::Request(error.to_string()))?;

        if response.status().as_u16() == 404 {
            return Ok(Vec::new());
        }
        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: parse_retry_after(response.headers()),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "LRCLib returned HTTP {}",
                response.status()
            )));
        }

        let records = response
            .json::<Vec<LrclibRecord>>()
            .map_err(|error| ProviderError::Parse(error.to_string()))?;

        let candidates = records
            .into_iter()
            .filter_map(|record| parse_record(record, track))
            .collect();

        Ok(candidates)
    }
}

#[derive(Debug, Deserialize)]
pub struct LrclibRecord {
    #[serde(default, rename = "trackName")]
    pub track_name: Option<String>,
    #[serde(default, rename = "artistName")]
    pub artist_name: Option<String>,
    #[serde(default, rename = "albumName")]
    pub album_name: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default, rename = "syncedLyrics")]
    pub synced_lyrics: Option<String>,
    #[serde(default, rename = "plainLyrics")]
    pub plain_lyrics: Option<String>,
    #[serde(default)]
    pub instrumental: Option<bool>,
}

fn parse_record(record: LrclibRecord, track: &Track) -> Option<Candidate> {
    let synced = non_empty(record.synced_lyrics);
    let plain = non_empty(record.plain_lyrics);
    if synced.is_none() && plain.is_none() {
        return None;
    }
    Some(Candidate {
        title: record.track_name.unwrap_or_else(|| track.title.clone()),
        artist: record.artist_name.unwrap_or_else(|| track.artist.clone()),
        album: non_empty(record.album_name),
        duration_seconds: record.duration,
        synced,
        plain,
        provider: ProviderName::Lrclib,
    })
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synced_and_plain_fields() {
        let payload: LrclibRecord = serde_json::from_str(
            r#"{"trackName":"Hello Song","artistName":"Hello Artist","albumName":"Hello Album","duration":200.0,"syncedLyrics":"[00:01]hello","plainLyrics":"hello","instrumental":false}"#,
        )
        .unwrap();
        let track = Track {
            audio_path: "test.mp3".to_string(),
            lrc_path: "test.lrc".to_string(),
            artist: "Fallback Artist".to_string(),
            title: "Fallback Title".to_string(),
            album: "".to_string(),
            duration_seconds: None,
            status: crate::model::LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };
        let cand = parse_record(payload, &track).unwrap();
        assert_eq!(cand.provider, ProviderName::Lrclib);
        assert_eq!(cand.title, "Hello Song");
        assert_eq!(cand.artist, "Hello Artist");
        assert_eq!(cand.album.as_deref(), Some("Hello Album"));
        assert_eq!(cand.duration_seconds, Some(200.0));
        assert_eq!(cand.synced.as_deref(), Some("[00:01]hello"));
        assert_eq!(cand.plain.as_deref(), Some("hello"));
    }

    #[test]
    fn treats_instrumental_response_as_empty_result() {
        let payload: LrclibRecord =
            serde_json::from_str(r#"{"syncedLyrics":null,"plainLyrics":null,"instrumental":true}"#)
                .unwrap();
        let track = Track {
            audio_path: "test.mp3".to_string(),
            lrc_path: "test.lrc".to_string(),
            artist: "Artist".to_string(),
            title: "Title".to_string(),
            album: "".to_string(),
            duration_seconds: None,
            status: crate::model::LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };
        assert!(parse_record(payload, &track).is_none());
    }

    #[test]
    fn parses_search_array_response() {
        let payload: Vec<LrclibRecord> = serde_json::from_str(
            r#"[
                {"trackName":"Song 1","artistName":"Artist 1","duration":180.0,"syncedLyrics":"[00:01]line1"},
                {"trackName":"Song 2","artistName":"Artist 2","duration":210.0,"plainLyrics":"plain lyrics"}
            ]"#,
        )
        .unwrap();
        let track = Track {
            audio_path: "test.mp3".to_string(),
            lrc_path: "test.lrc".to_string(),
            artist: "Fallback Artist".to_string(),
            title: "Fallback Title".to_string(),
            album: "".to_string(),
            duration_seconds: None,
            status: crate::model::LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };
        let cands: Vec<Candidate> = payload
            .into_iter()
            .filter_map(|r| parse_record(r, &track))
            .collect();
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0].title, "Song 1");
        assert_eq!(cands[0].synced.as_deref(), Some("[00:01]line1"));
        assert_eq!(cands[1].title, "Song 2");
        assert_eq!(cands[1].plain.as_deref(), Some("plain lyrics"));
    }
}
