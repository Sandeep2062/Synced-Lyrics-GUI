use super::{LyricsProvider, ProviderError, ProviderName};
use crate::matching::Candidate;
use crate::model::Track;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;

static LYRICS_CONTAINER_SELECTOR: OnceLock<Option<Selector>> = OnceLock::new();
static EXCLUDE_FROM_SELECTION_SELECTOR: OnceLock<Option<Selector>> = OnceLock::new();

fn lyrics_container_selector() -> Option<&'static Selector> {
    LYRICS_CONTAINER_SELECTOR
        .get_or_init(|| Selector::parse("[data-lyrics-container='true']").ok())
        .as_ref()
}

fn exclude_selector() -> Option<&'static Selector> {
    EXCLUDE_FROM_SELECTION_SELECTOR
        .get_or_init(|| Selector::parse("[data-exclude-from-selection='true']").ok())
        .as_ref()
}

const API_URL: &str = "https://api.genius.com/search";
const PUBLIC_SEARCH_URL: &str = "https://genius.com/api/search/multi";
const BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const GENIUS_COOKIE: &str = "obuid=e3ee67e0-7df9-4181-8324-d977c6dc9250";

const TOP_N: usize = 3;

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

    fn search_public(&self, track: &Track) -> Result<Vec<Candidate>, ProviderError> {
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

        let hits = multi
            .response
            .and_then(|r| r.sections)
            .and_then(|sections| {
                sections
                    .into_iter()
                    .find(|s| s.section_type.as_deref() == Some("song"))
                    .and_then(|s| s.hits)
            })
            .unwrap_or_default();

        let mut candidates = Vec::new();
        for hit in hits.into_iter().take(TOP_N) {
            let cand_title = hit.result.title.unwrap_or_else(|| track.title.clone());
            let cand_artist = hit
                .result
                .artist_names
                .or_else(|| hit.result.primary_artist.and_then(|a| a.name))
                .unwrap_or_else(|| track.artist.clone());

            if let Ok(Some(lyrics)) = self.fetch_lyrics_from_page(&hit.result.url) {
                candidates.push(Candidate {
                    title: cand_title,
                    artist: cand_artist,
                    album: None,
                    duration_seconds: None,
                    synced: None,
                    plain: Some(lyrics),
                    provider: ProviderName::Genius,
                });
            }
        }

        Ok(candidates)
    }

    fn search_with_token(&self, track: &Track) -> Result<Vec<Candidate>, ProviderError> {
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

        let hits = search
            .response
            .map(|response| response.hits)
            .unwrap_or_default();

        let mut candidates = Vec::new();
        for hit in hits.into_iter().take(TOP_N) {
            let cand_title = hit.result.title.unwrap_or_else(|| track.title.clone());
            let cand_artist = hit
                .result
                .artist_names
                .or_else(|| hit.result.primary_artist.and_then(|a| a.name))
                .unwrap_or_else(|| track.artist.clone());

            if let Ok(Some(lyrics)) = self.fetch_lyrics_from_page(&hit.result.url) {
                candidates.push(Candidate {
                    title: cand_title,
                    artist: cand_artist,
                    album: None,
                    duration_seconds: None,
                    synced: None,
                    plain: Some(lyrics),
                    provider: ProviderName::Genius,
                });
            }
        }

        Ok(candidates)
    }

    fn fetch_lyrics_from_page(&self, url: &str) -> Result<Option<String>, ProviderError> {
        let page = self
            .client
            .get(url)
            .header(reqwest::header::USER_AGENT, BROWSER_USER_AGENT)
            .send()
            .map_err(|error| ProviderError::Request(error.to_string()))?
            .text()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        let lyrics = extract_lyrics(&page);
        Ok((!lyrics.is_empty()).then_some(lyrics))
    }
}

impl LyricsProvider for GeniusProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Genius
    }

    fn search(&self, track: &Track) -> Result<Vec<Candidate>, ProviderError> {
        if self.token.trim().is_empty() {
            self.search_public(track)
        } else {
            self.search_with_token(track)
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    pub response: Option<SearchBody>,
}

#[derive(Debug, Deserialize)]
pub struct SearchBody {
    pub hits: Vec<Hit>,
}

#[derive(Debug, Deserialize)]
pub struct MultiSearchResponse {
    pub response: Option<MultiSearchBody>,
}

#[derive(Debug, Deserialize)]
pub struct MultiSearchBody {
    pub sections: Option<Vec<MultiSearchSection>>,
}

#[derive(Debug, Deserialize)]
pub struct MultiSearchSection {
    #[serde(rename = "type")]
    pub section_type: Option<String>,
    pub hits: Option<Vec<Hit>>,
}

#[derive(Debug, Deserialize)]
pub struct Hit {
    pub result: HitResult,
}

#[derive(Debug, Deserialize)]
pub struct HitResult {
    pub url: String,
    pub title: Option<String>,
    pub artist_names: Option<String>,
    pub primary_artist: Option<GeniusArtist>,
}

#[derive(Debug, Deserialize)]
pub struct GeniusArtist {
    pub name: Option<String>,
}

fn extract_lyrics(page: &str) -> String {
    let document = Html::parse_document(page);
    let Some(selector) = lyrics_container_selector() else {
        return String::new();
    };
    let exclude_sel = exclude_selector();

    document
        .select(selector)
        .filter_map(|container| {
            let mut html = container.inner_html();
            if let Some(ex_sel) = exclude_sel {
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
    fn parses_real_fixtures() {
        let multi_json = include_str!("../../tests/fixtures/genius/bohemian_rhapsody_search.json");
        let payload: MultiSearchResponse = serde_json::from_str(multi_json).unwrap();
        let sections = payload.response.unwrap().sections.unwrap();
        let song_sec = sections.into_iter().find(|s| s.section_type.as_deref() == Some("song")).unwrap();
        let hits = song_sec.hits.unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].result.title.as_deref(), Some("Bohemian Rhapsody"));

        let html = include_str!("../../tests/fixtures/genius/lyrics_container.html");
        let lyrics = extract_lyrics(html);
        assert!(!lyrics.is_empty());
    }

    #[test]
    fn parses_search_response_and_lyrics_containers() {
        let response: SearchResponse = serde_json::from_str(
            r#"{"response":{"hits":[{"result":{"url":"https://example.test/song","title":"Song 1","artist_names":"Artist 1"}}]}}"#,
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
            <span data-exclude-from-selection="true">Contributor Notes</span>
            [Verse 1]<br>Line 1<br>Line 2
        </div>"#;
        assert_eq!(extract_lyrics(snippet), "[Verse 1]\nLine 1\nLine 2");
    }

    #[test]
    fn parses_multi_search_response() {
        let payload: MultiSearchResponse = serde_json::from_str(
            r#"{
                "response": {
                    "sections": [
                        {
                            "type": "top_hit",
                            "hits": []
                        },
                        {
                            "type": "song",
                            "hits": [
                                {
                                    "result": {
                                        "url": "https://genius.com/test1",
                                        "title": "First Song",
                                        "artist_names": "First Artist"
                                    }
                                },
                                {
                                    "result": {
                                        "url": "https://genius.com/test2",
                                        "title": "Second Song",
                                        "artist_names": "Second Artist"
                                    }
                                }
                            ]
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let hits = payload
            .response
            .unwrap()
            .sections
            .unwrap()
            .into_iter()
            .find(|s| s.section_type.as_deref() == Some("song"))
            .unwrap()
            .hits
            .unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].result.title.as_deref(), Some("First Song"));
        assert_eq!(hits[0].result.artist_names.as_deref(), Some("First Artist"));
        assert_eq!(hits[1].result.title.as_deref(), Some("Second Song"));
    }

    #[test]
    #[ignore = "requires network"]
    fn live_genius_public_search() {
        let provider = GeniusProvider::new("").unwrap();
        let track = Track {
            audio_path: "test.mp3".to_string(),
            lrc_path: "test.lrc".to_string(),
            artist: "Lil Peep".to_string(),
            title: "Rockstar".to_string(),
            album: "".to_string(),
            duration_seconds: None,
            status: crate::model::LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };
        let result = provider.search(&track).unwrap();
        assert!(!result.is_empty());
        let lyrics = &result[0];
        assert!(lyrics.plain.is_some());
        assert!(!lyrics.plain.as_ref().unwrap().is_empty());
    }

    #[test]
    fn test_malformed_genius_html_and_json() {
        // HTML without lyrics container
        let html_no_container = "<html><body><div>No lyrics here</div></body></html>";
        assert_eq!(extract_lyrics(html_no_container), "");

        // Empty HTML
        assert_eq!(extract_lyrics(""), "");

        // Corrupted HTML with unclosed tags and nested scripts
        let html_corrupted = "<div data-lyrics-container=\"true\"><script>var x = 1;</script>Line 1<br/>Line 2</div>";
        let extracted = extract_lyrics(html_corrupted);
        assert!(extracted.contains("Line 1"));

        // Malformed Search JSON
        let malformed_jsons = [
            "{}",
            r#"{"response": null}"#,
            r#"{"response": {"hits": null}}"#,
            r#"{"response": {"hits": []}}"#,
            r#"{"response": {"sections": [{"type": "other"}]}}"#,
        ];

        for json_str in &malformed_jsons {
            let _ = serde_json::from_str::<SearchResponse>(json_str);
            let _ = serde_json::from_str::<MultiSearchResponse>(json_str);
        }
    }
}
