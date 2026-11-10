use super::{LyricsProvider, ProviderError, ProviderName};
use crate::matching::Candidate;
use crate::model::Track;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;

const SEARCH_URL: &str = "https://api.megalobiz.com/api/public/v1/search";
const LRC_BASE_URL: &str = "https://api.megalobiz.com/api/public/v1/lrc";
const TOP_N: usize = 3;

pub struct MegalobizProvider {
    client: Client,
    search_url: String,
    lrc_base_url: String,
}

impl MegalobizProvider {
    pub fn new() -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("SyncedLyricsGUI/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        Ok(Self {
            client,
            search_url: SEARCH_URL.to_string(),
            lrc_base_url: LRC_BASE_URL.to_string(),
        })
    }

    pub fn from_client(client: Client) -> Self {
        Self {
            client,
            search_url: SEARCH_URL.to_string(),
            lrc_base_url: LRC_BASE_URL.to_string(),
        }
    }

    pub fn with_urls(
        client: Client,
        search_url: impl Into<String>,
        lrc_base_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            search_url: search_url.into(),
            lrc_base_url: lrc_base_url.into(),
        }
    }
}

impl LyricsProvider for MegalobizProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Megalobiz
    }

    fn search(&self, track: &Track) -> Result<Vec<Candidate>, ProviderError> {
        let has_title_artist = !track.title.trim().is_empty() && !track.artist.trim().is_empty();
        let duration_str = track.duration_seconds.map(|d| format!("{:.0}", d));

        let mut req = self.client.get(&self.search_url);
        if has_title_artist {
            req = req.query(&[
                ("title", track.title.trim()),
                ("artist", track.artist.trim()),
            ]);
            if let Some(dur) = &duration_str {
                req = req.query(&[("duration", dur.as_str())]);
            }
        } else {
            let q = format!("{} {}", track.search_artist(), track.search_title());
            req = req.query(&[("q", q.trim())]);
            if let Some(dur) = &duration_str {
                req = req.query(&[("duration", dur.as_str())]);
            }
        }

        let resp = req
            .send()
            .map_err(|e| ProviderError::Request(e.to_string()))?;

        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: parse_retry_after(resp.headers()),
            });
        }
        if resp.status().as_u16() == 404 {
            return Ok(Vec::new());
        }
        if !resp.status().is_success() {
            return Err(ProviderError::Request(format!(
                "Megalobiz search returned HTTP {}",
                resp.status()
            )));
        }

        let search_data = resp
            .json::<MegalobizSearchResponse>()
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        let Some(results) = search_data.results else {
            return Ok(Vec::new());
        };

        let mut candidates = Vec::new();
        for item in results.into_iter().take(TOP_N) {
            let lrc_endpoint = format!("{}/{}", self.lrc_base_url.trim_end_matches('/'), item.id);
            let lrc_resp = match self.client.get(&lrc_endpoint).send() {
                Ok(r) => r,
                Err(_) => continue,
            };

            if lrc_resp.status().as_u16() == 429 {
                return Err(ProviderError::RateLimited {
                    retry_after_seconds: parse_retry_after(lrc_resp.headers()),
                });
            }
            if !lrc_resp.status().is_success() {
                continue;
            }

            let Ok(lrc_data) = lrc_resp.json::<MegalobizLrcResponse>() else {
                continue;
            };

            if let Some(lrc_text) = lrc_data.lrc.filter(|t| !t.trim().is_empty()) {
                candidates.push(Candidate {
                    title: item
                        .title
                        .or(lrc_data.title)
                        .unwrap_or_else(|| track.title.clone()),
                    artist: item
                        .artist
                        .or(lrc_data.artist)
                        .unwrap_or_else(|| track.artist.clone()),
                    album: None,
                    duration_seconds: item.duration.or(lrc_data.duration),
                    synced: Some(lrc_text),
                    plain: None,
                    provider: ProviderName::Megalobiz,
                });
            }
        }

        Ok(candidates)
    }
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

#[derive(Debug, Deserialize)]
pub struct MegalobizSearchResponse {
    #[serde(default)]
    pub count: Option<usize>,
    #[serde(default)]
    pub results: Option<Vec<MegalobizSearchResult>>,
}

#[derive(Debug, Deserialize)]
pub struct MegalobizSearchResult {
    pub id: u64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub lrc_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MegalobizLrcResponse {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub lrc: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_megalobiz_search_and_lrc_fixtures() {
        let search_json =
            include_str!("../../tests/fixtures/megalobiz/bohemian_rhapsody_search.json");
        let search_res: MegalobizSearchResponse = serde_json::from_str(search_json).unwrap();
        assert_eq!(search_res.count, Some(10));
        let results = search_res.results.unwrap();
        assert_eq!(results.len(), 10);
        assert_eq!(results[0].id, 56215);
        assert_eq!(results[0].title.as_deref(), Some("Bohemian Rhapsody"));
        assert_eq!(results[0].artist.as_deref(), Some("Queen"));

        let lrc_json = include_str!("../../tests/fixtures/megalobiz/bohemian_rhapsody_lrc.json");
        let lrc_res: MegalobizLrcResponse = serde_json::from_str(lrc_json).unwrap();
        assert_eq!(lrc_res.id, Some(56215));
        let lrc = lrc_res.lrc.unwrap();
        assert!(lrc.contains("[00:00.26]Is this the real life?"));
    }

    #[test]
    fn handles_empty_or_malformed_json() {
        let malformed = [
            "{}",
            r#"{"results": []}"#,
            r#"{"count": 0, "results": null}"#,
        ];
        for s in malformed {
            let res = serde_json::from_str::<MegalobizSearchResponse>(s);
            assert!(res.is_ok());
        }
    }
}
