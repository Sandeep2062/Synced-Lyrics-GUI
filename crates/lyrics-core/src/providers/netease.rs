use super::{LyricsProvider, ProviderError, ProviderLyrics, ProviderName};
use crate::model::Track;
use reqwest::blocking::Client;
use std::time::Duration;

const SEARCH_URL: &str = "https://music.163.com/api/search/pc";
const LYRICS_URL: &str = "https://music.163.com/api/song/lyric";
const REFERER: &str = "https://music.163.com/";
const NETEASE_COOKIE: &str = "NMTID=00OAVK3xqDG726ITU6jopU6jF2yMk0AAAGCO8l1BA; JSESSIONID-WYYY=8KQo11YK2GZP45RMlz8Kn80vHZ9%2FGvwzRKQXXy0iQoFKycWdBlQjbfT0MJrFa6hwRfmpfBYKeHliUPH287JC3hNW99WQjrh9b9RmKT%2Fg1Exc2VwHZcsqi7ITxQgfEiee50po28x5xTTZXKoP%2FRMctN2jpDeg57kdZrXz%2FD%2FWghb%5C4DuZ%3A1659124633932; _iuqxldmzr_=32; _ntes_nnid=0db6667097883aa9596ecfe7f188c3ec,1659122833973; _ntes_nuid=0db6667097883aa9596ecfe7f188c3ec; WNMCID=xygast.1659122837568.01.0; WEVNSM=1.0.0; WM_NI=CwbjWAFbcIzPX3dsLP%2F52VB%2Bxr572gmqAYwvN9KU5X5f1nRzBYl0SNf%2BV9FTmmYZy%2FoJLADaZS0Q8TrKfNSBNOt0HLB8rRJh9DsvMOT7%2BCGCQLbvlWAcJBJeXb1P8yZ3RHA%3D; WM_NIKE=9ca17ae2e6ffcda170e2e6ee90c65b85ae87b9aa5483ef8ab3d14a939e9a83c459959caeadce47e991fbaee82af0fea7c3b92a81a9ae8bd64b86beadaaf95c9cedac94cf5cedebfeb7c121bcaefbd8b16dafaf8fbaf67e8ee785b6b854f7baff8fd1728287a4d1d246a6f59adac560afb397bbfc25ad9684a2c76b9a8d00b2bb60b295aaafd24a8e91bcd1cb4882e8beb3c964fb9cbd97d04598e9e5a4c6499394ae97ef5d83bd86a3c96f9cbeffb1bb739aed9ea9c437e2a3; WM_TID=AAkRFnl03RdABEBEQFOBWHCPOeMra4IL; playerid=94262567";

pub struct NeteaseProvider {
    client: Client,
    search_url: String,
    lyrics_url: String,
}

impl NeteaseProvider {
    pub fn new() -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        Ok(Self {
            client,
            search_url: SEARCH_URL.to_string(),
            lyrics_url: LYRICS_URL.to_string(),
        })
    }

    pub fn from_client(
        client: Client,
        search_url: impl Into<String>,
        lyrics_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            search_url: search_url.into(),
            lyrics_url: lyrics_url.into(),
        }
    }
}

impl LyricsProvider for NeteaseProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Netease
    }

    fn search(&self, track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
        let query = format!("{} {}", track.title, track.artist);
        let response = self
            .client
            .get(&self.search_url)
            .header(reqwest::header::COOKIE, NETEASE_COOKIE)
            .header(reqwest::header::REFERER, REFERER)
            .query(&[
                ("s", query.as_str()),
                ("type", "1"),
                ("limit", "10"),
                ("offset", "0"),
            ])
            .send()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: Some(60),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "NetEase search returned HTTP {}",
                response.status()
            )));
        }
        let search = response
            .json::<serde_json::Value>()
            .map_err(|error| ProviderError::Parse(error.to_string()))?;
        let Some(songs) = search
            .get("result")
            .and_then(|r| r.get("songs"))
            .and_then(|s| s.as_array())
        else {
            return Ok(None);
        };
        let Some(song_id) = songs
            .first()
            .and_then(|s| s.get("id"))
            .and_then(|id| id.as_u64())
        else {
            return Ok(None);
        };

        let response = self
            .client
            .get(&self.lyrics_url)
            .header(reqwest::header::COOKIE, NETEASE_COOKIE)
            .header(reqwest::header::REFERER, REFERER)
            .query(&[
                ("id", song_id.to_string().as_str()),
                ("lv", "1"),
                ("kv", "1"),
                ("tv", "-1"),
            ])
            .send()
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        if response.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: Some(60),
            });
        }
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "NetEase lyric request returned HTTP {}",
                response.status()
            )));
        }
        let lyrics = response
            .json::<serde_json::Value>()
            .map_err(|error| ProviderError::Parse(error.to_string()))?;
        let Some(synced) = lyrics
            .get("lrc")
            .and_then(|l| l.get("lyric"))
            .and_then(|t| t.as_str())
            .map(|t| t.to_string())
            .filter(|text| !text.trim().is_empty())
        else {
            return Ok(None);
        };
        Ok(Some(ProviderLyrics {
            provider: ProviderName::Netease,
            plain: Some(strip_timestamps(&synced)),
            synced: Some(synced),
            confidence: 0.8,
        }))
    }
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
    fn parses_search_and_lyrics_payloads() {
        let search: serde_json::Value =
            serde_json::from_str(r#"{"result":{"songs":[{"id":42}]}}"#).unwrap();
        let song_id = search
            .get("result")
            .and_then(|r| r.get("songs"))
            .and_then(|s| s.as_array())
            .and_then(|s| s.first())
            .and_then(|s| s.get("id"))
            .and_then(|id| id.as_u64());
        assert_eq!(song_id, Some(42));
        let lyrics: serde_json::Value =
            serde_json::from_str(r#"{"lrc":{"lyric":"[00:01]Hello\n[00:02]World"}}"#).unwrap();
        let text = lyrics
            .get("lrc")
            .and_then(|l| l.get("lyric"))
            .and_then(|t| t.as_str())
            .unwrap();
        assert_eq!(strip_timestamps(text), "Hello\nWorld");
    }

    #[test]
    #[ignore = "requires network"]
    fn live_netease_search() {
        let provider = NeteaseProvider::new().unwrap();
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
        };
        let result = provider.search(&track).unwrap();
        assert!(result.is_some());
        let lyrics = result.unwrap();
        assert!(lyrics.synced.is_some());
        assert!(lyrics.synced.unwrap().contains("00:"));
    }
}
