use super::{LyricsProvider, ProviderError, ProviderLyrics, ProviderName};
use crate::model::Track;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OFFICIAL_API_URL: &str = "https://api.musixmatch.com/ws/1.1";
const DESKTOP_API_URL: &str = "https://apic-desktop.musixmatch.com/ws/1.1";
const APP_ID: &str = "web-desktop-app-v1.0";

pub struct MusixmatchProvider {
    client: Client,
    api_key: String,
    api_url: String,
    token_cache: Mutex<Option<(String, Instant)>>,
}

impl MusixmatchProvider {
    pub fn new(api_key: impl Into<String>) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
            .build()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        Ok(Self {
            client,
            api_key: api_key.into(),
            api_url: OFFICIAL_API_URL.to_string(),
            token_cache: Mutex::new(None),
        })
    }

    pub fn from_client(
        client: Client,
        api_key: impl Into<String>,
        api_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            api_key: api_key.into(),
            api_url: api_url.into(),
            token_cache: Mutex::new(None),
        }
    }

    fn current_time_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn get_user_token(&self) -> Result<String, ProviderError> {
        if let Ok(guard) = self.token_cache.lock() {
            if let Some((token, expires_at)) = &*guard {
                if Instant::now() < *expires_at {
                    return Ok(token.clone());
                }
            }
        }

        let now_str = Self::current_time_ms().to_string();
        let url = format!("{DESKTOP_API_URL}/token.get");
        let response = self
            .client
            .get(&url)
            .query(&[
                ("user_language", "en"),
                ("app_id", APP_ID),
                ("t", now_str.as_str()),
            ])
            .send()
            .map_err(|e| ProviderError::Request(e.to_string()))?;

        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: Some(60),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "Musixmatch token.get returned HTTP {}",
                response.status()
            )));
        }

        let payload = response
            .json::<TokenResponse>()
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        let token = payload
            .message
            .and_then(|m| m.body)
            .and_then(|b| b.user_token)
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| ProviderError::Parse("Missing user_token in response".to_string()))?;

        if let Ok(mut guard) = self.token_cache.lock() {
            // Expire token after 9 minutes (official TTL is 10)
            *guard = Some((token.clone(), Instant::now() + Duration::from_secs(540)));
        }

        Ok(token)
    }

    fn search_builtin(&self, track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
        let token = self.get_user_token()?;
        let now_str = Self::current_time_ms().to_string();
        let query_str = format!("{} {}", track.title, track.artist);

        // Step 1: Search track
        let search_url = format!("{DESKTOP_API_URL}/track.search");
        let response = self
            .client
            .get(&search_url)
            .query(&[
                ("q", query_str.as_str()),
                ("page_size", "5"),
                ("page", "1"),
                ("app_id", APP_ID),
                ("usertoken", token.as_str()),
                ("t", now_str.as_str()),
            ])
            .send()
            .map_err(|e| ProviderError::Request(e.to_string()))?;

        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: Some(60),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "Musixmatch track.search returned HTTP {}",
                response.status()
            )));
        }

        let search_payload = response
            .json::<DesktopSearchResponse>()
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        let track_id = search_payload
            .message
            .and_then(|m| m.body)
            .and_then(|b| b.track_list)
            .and_then(|list| list.into_iter().next())
            .and_then(|item| item.track)
            .and_then(|t| t.track_id);

        let Some(track_id) = track_id else {
            return Ok(None);
        };

        // Step 2: Get subtitle (LRC)
        let now_str = Self::current_time_ms().to_string();
        let sub_url = format!("{DESKTOP_API_URL}/track.subtitle.get");
        let response = self
            .client
            .get(&sub_url)
            .query(&[
                ("track_id", track_id.to_string().as_str()),
                ("subtitle_format", "lrc"),
                ("app_id", APP_ID),
                ("usertoken", token.as_str()),
                ("t", now_str.as_str()),
            ])
            .send()
            .map_err(|e| ProviderError::Request(e.to_string()))?;

        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: Some(60),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "Musixmatch track.subtitle.get returned HTTP {}",
                response.status()
            )));
        }

        let sub_payload = response
            .json::<ApiResponse>()
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        if let Some(text) = sub_payload
            .message
            .and_then(|m| m.body)
            .and_then(|b| b.subtitle)
            .and_then(|s| s.subtitle_body)
            .filter(|t| !t.trim().is_empty())
        {
            return Ok(Some(ProviderLyrics {
                provider: ProviderName::Musixmatch,
                plain: Some(strip_timestamps(&text)),
                synced: Some(text),
                confidence: 0.9,
            }));
        }

        Ok(None)
    }

    fn search_api_key(&self, track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
        let query = [
            ("q_track", track.title.as_str()),
            ("q_artist", track.artist.as_str()),
            ("apikey", self.api_key.as_str()),
        ];
        let subtitle = self.request("matcher.subtitle.get", &query)?;
        if let Some(text) = subtitle
            .message
            .and_then(|message| message.body)
            .and_then(|body| body.subtitle)
            .and_then(|subtitle| subtitle.subtitle_body)
            .filter(|text| !text.trim().is_empty())
        {
            return Ok(Some(ProviderLyrics {
                provider: ProviderName::Musixmatch,
                plain: Some(strip_timestamps(&text)),
                synced: Some(text),
                confidence: 0.9,
            }));
        }

        let lyrics = self.request("matcher.lyrics.get", &query)?;
        let plain = lyrics
            .message
            .and_then(|message| message.body)
            .and_then(|body| body.lyrics)
            .and_then(|lyrics| lyrics.lyrics_body)
            .filter(|text| !text.trim().is_empty());
        Ok(plain.map(|plain| ProviderLyrics {
            provider: ProviderName::Musixmatch,
            synced: None,
            plain: Some(plain),
            confidence: 0.7,
        }))
    }
}

impl LyricsProvider for MusixmatchProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Musixmatch
    }

    fn search(&self, track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
        if self.api_key.trim().is_empty() {
            self.search_builtin(track)
        } else {
            self.search_api_key(track)
        }
    }
}

impl MusixmatchProvider {
    fn request(
        &self,
        endpoint: &str,
        query: &[(&str, &str)],
    ) -> Result<ApiResponse, ProviderError> {
        let response = self
            .client
            .get(format!("{}/{}", self.api_url, endpoint))
            .query(query)
            .send()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: Some(60),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "Musixmatch returned HTTP {}",
                response.status()
            )));
        }
        let payload = response
            .json::<ApiResponse>()
            .map_err(|error| ProviderError::Parse(error.to_string()))?;
        if payload
            .message
            .as_ref()
            .and_then(|message| message.header.as_ref())
            .and_then(|header| header.status_code)
            == Some(429)
        {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: Some(60),
            });
        }
        Ok(payload)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    message: Option<TokenMessage>,
}

#[derive(Debug, Deserialize)]
struct TokenMessage {
    body: Option<TokenBody>,
}

#[derive(Debug, Deserialize)]
struct TokenBody {
    user_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DesktopSearchResponse {
    message: Option<DesktopSearchMessage>,
}

#[derive(Debug, Deserialize)]
struct DesktopSearchMessage {
    body: Option<DesktopSearchBody>,
}

#[derive(Debug, Deserialize)]
struct DesktopSearchBody {
    track_list: Option<Vec<DesktopTrackItem>>,
}

#[derive(Debug, Deserialize)]
struct DesktopTrackItem {
    track: Option<DesktopTrack>,
}

#[derive(Debug, Deserialize)]
struct DesktopTrack {
    track_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    message: Option<ApiMessage>,
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    header: Option<ApiHeader>,
    body: Option<ApiBody>,
}

#[derive(Debug, Deserialize)]
struct ApiHeader {
    status_code: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct ApiBody {
    subtitle: Option<Subtitle>,
    lyrics: Option<Lyrics>,
}

#[derive(Debug, Deserialize)]
struct Subtitle {
    subtitle_body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Lyrics {
    lyrics_body: Option<String>,
}

fn strip_timestamps(text: &str) -> String {
    text.lines()
        .map(|line| {
            let mut remaining = line;
            while remaining.starts_with('[') {
                let Some(end) = remaining.find(']') else {
                    break;
                };
                remaining = &remaining[end + 1..];
            }
            remaining.trim()
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subtitle_and_plain_response_shapes() {
        let payload: ApiResponse = serde_json::from_str(
            r#"{"message":{"header":{"status_code":200},"body":{"subtitle":{"subtitle_body":"[00:01]Hello"}}}}"#,
        )
        .unwrap();
        let text = payload
            .message
            .unwrap()
            .body
            .unwrap()
            .subtitle
            .unwrap()
            .subtitle_body
            .unwrap();
        assert_eq!(strip_timestamps(&text), "Hello");
    }

    #[test]
    fn parses_token_response() {
        let payload: TokenResponse = serde_json::from_str(
            r#"{"message":{"header":{"status_code":200},"body":{"user_token":"sample_token_xyz"}}}"#,
        )
        .unwrap();
        assert_eq!(
            payload.message.unwrap().body.unwrap().user_token.unwrap(),
            "sample_token_xyz"
        );
    }
}
