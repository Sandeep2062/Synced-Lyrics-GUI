use super::{LyricsProvider, ProviderError, ProviderLyrics, ProviderName};
use crate::model::Track;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use std::time::Duration;

const API_URL: &str = "https://api.genius.com/search";
const PUBLIC_SEARCH_URL: &str = "https://genius.com/api/search/multi";
const BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const GENIUS_COOKIE: &str = "obuid=e3ee67e0-7df9-4181-8324-d977c6dc9250";

pub struct GeniusProvider {
    client: Client,
    token: String,
}

impl GeniusProvider {
    pub fn new(token: impl Into<String>) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(BROWSER_USER_AGENT)
            .build()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        Ok(Self {
            client,
            token: token.into(),
        })
    }

    pub fn from_client(client: Client, token: impl Into<String>) -> Self {
        Self {
            client,
            token: token.into(),
        }
    }

    fn search_public(&self, track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
        let query = format!("{} {}", track.title, track.artist);
        let response = self
            .client
            .get(PUBLIC_SEARCH_URL)
            .header(reqwest::header::USER_AGENT, BROWSER_USER_AGENT)
            .header(reqwest::header::COOKIE, GENIUS_COOKIE)
            .query(&[("q", query.as_str()), ("per_page", "5")])
            .send()
            .map_err(|error| ProviderError::Request(error.to_string()))?;

        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: Some(60),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "Genius public API returned HTTP {}",
                response.status()
            )));
        }

        let multi = response
            .json::<MultiSearchResponse>()
            .map_err(|error| ProviderError::Parse(error.to_string()))?;

        let url = multi
            .response
            .and_then(|r| r.sections)
            .and_then(|sections| {
                sections
                    .into_iter()
                    .find(|s| s.section_type.as_deref() == Some("song"))
                    .and_then(|s| s.hits)
                    .and_then(|hits| hits.into_iter().next())
                    .map(|hit| hit.result.url)
            });

        let Some(url) = url else {
            return Ok(None);
        };

        self.fetch_lyrics_from_page(&url)
    }

    fn search_with_token(&self, track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
        let query = format!("{} {}", track.title, track.artist);
        let response = self
            .client
            .get(API_URL)
            .bearer_auth(&self.token)
            .header(reqwest::header::USER_AGENT, BROWSER_USER_AGENT)
            .query(&[("q", query.as_str())])
            .send()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: Some(60),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "Genius returned HTTP {}",
                response.status()
            )));
        }
        let search = response
            .json::<SearchResponse>()
            .map_err(|error| ProviderError::Parse(error.to_string()))?;
        let Some(url) = search
            .response
            .and_then(|response| response.hits.into_iter().next())
            .map(|hit| hit.result.url)
        else {
            return Ok(None);
        };
        self.fetch_lyrics_from_page(&url)
    }

    fn fetch_lyrics_from_page(&self, url: &str) -> Result<Option<ProviderLyrics>, ProviderError> {
        let page = self
            .client
            .get(url)
            .header(reqwest::header::USER_AGENT, BROWSER_USER_AGENT)
            .send()
            .map_err(|error| ProviderError::Request(error.to_string()))?
            .text()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        let lyrics = extract_lyrics(&page);
        Ok((!lyrics.is_empty()).then_some(ProviderLyrics {
            provider: ProviderName::Genius,
            synced: None,
            plain: Some(lyrics),
            confidence: 0.7,
        }))
    }
}

impl LyricsProvider for GeniusProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Genius
    }

    fn search(&self, track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
        if self.token.trim().is_empty() {
            self.search_public(track)
        } else {
            self.search_with_token(track)
        }
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    response: Option<SearchBody>,
}

#[derive(Debug, Deserialize)]
struct SearchBody {
    hits: Vec<Hit>,
}

#[derive(Debug, Deserialize)]
struct MultiSearchResponse {
    response: Option<MultiSearchBody>,
}

#[derive(Debug, Deserialize)]
struct MultiSearchBody {
    sections: Option<Vec<MultiSearchSection>>,
}

#[derive(Debug, Deserialize)]
struct MultiSearchSection {
    #[serde(rename = "type")]
    section_type: Option<String>,
    hits: Option<Vec<Hit>>,
}

#[derive(Debug, Deserialize)]
struct Hit {
    result: HitResult,
}

#[derive(Debug, Deserialize)]
struct HitResult {
    url: String,
}

fn extract_lyrics(page: &str) -> String {
    let document = Html::parse_document(page);
    let Ok(selector) = Selector::parse("[data-lyrics-container='true']") else {
        return String::new();
    };
    let exclude_sel = Selector::parse("[data-exclude-from-selection='true']").ok();

    document
        .select(&selector)
        .filter_map(|container| {
            let mut html = container.inner_html();
            if let Some(ref ex_sel) = exclude_sel {
                for ex in container.select(ex_sel) {
                    let ex_html = ex.html();
                    html = html.replace(&ex_html, "");
                }
            }
            let html = html
                .replace("<br>", "\n")
                .replace("<br/>", "\n")
                .replace("<br />", "\n");
            let text = Html::parse_fragment(&html)
                .root_element()
                .text()
                .collect::<Vec<_>>()
                .join("");
            let trimmed = text.trim();
            if trimmed.is_empty() || trimmed == "[Music Video]" {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_response_and_lyrics_containers() {
        let response: SearchResponse = serde_json::from_str(
            r#"{"response":{"hits":[{"result":{"url":"https://example.test/song"}}]}}"#,
        )
        .unwrap();
        assert_eq!(
            response.response.unwrap().hits[0].result.url,
            "https://example.test/song"
        );
        assert_eq!(
            extract_lyrics("<div data-lyrics-container='true'>Hello<br>World</div>"),
            "Hello\nWorld"
        );
    }

    #[test]
    fn cleans_excluded_header_elements_from_lyrics() {
        let snippet = r#"<div data-lyrics-container="true">
            <div data-exclude-from-selection="true"><span>234 Contributors</span><span>Translations</span></div>
            [Verse 1]<br>Hello from the other side
        </div>"#;
        assert_eq!(
            extract_lyrics(snippet),
            "[Verse 1]\nHello from the other side"
        );
    }

    #[test]
    fn parses_multi_search_response() {
        let json = r#"{
            "response": {
                "sections": [
                    { "type": "top_hit", "hits": [] },
                    { "type": "song", "hits": [
                        { "result": { "url": "https://genius.com/Adele-hello-lyrics" } }
                    ]}
                ]
            }
        }"#;
        let multi: MultiSearchResponse = serde_json::from_str(json).unwrap();
        let song_url = multi
            .response
            .and_then(|r| r.sections)
            .and_then(|sections| {
                sections
                    .into_iter()
                    .find(|s| s.section_type.as_deref() == Some("song"))
                    .and_then(|s| s.hits)
                    .and_then(|hits| hits.into_iter().next())
                    .map(|hit| hit.result.url)
            });
        assert_eq!(
            song_url,
            Some("https://genius.com/Adele-hello-lyrics".to_string())
        );
    }

    #[test]
    #[ignore = "requires network"]
    fn live_genius_public_search() {
        let provider = GeniusProvider::new("").unwrap();
        let track = Track {
            audio_path: "test.mp3".to_string(),
            lrc_path: "test.lrc".to_string(),
            artist: "Adele".to_string(),
            title: "Hello".to_string(),
            album: "25".to_string(),
            duration_seconds: Some(295.0),
            status: crate::model::LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
        };
        let res = provider.search(&track).unwrap();
        assert!(res.is_some());
        let lyrics = res.unwrap();
        assert!(lyrics.plain.is_some());
        assert!(lyrics.plain.unwrap().contains("Hello, it's me"));
    }
}
