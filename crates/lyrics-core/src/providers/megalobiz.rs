use super::{LyricsProvider, ProviderError, ProviderName};
use crate::matching::Candidate;
use crate::model::Track;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::time::Duration;

const SEARCH_URL: &str = "https://www.megalobiz.com/search/all";
const TOP_N: usize = 3;

pub struct MegalobizProvider {
    client: Client,
}

impl MegalobizProvider {
    pub fn new() -> Result<Self, ProviderError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(6))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        Ok(Self { client })
    }

    pub fn from_client(client: Client) -> Self {
        Self { client }
    }
}

impl LyricsProvider for MegalobizProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Megalobiz
    }

    fn search(&self, track: &Track) -> Result<Vec<Candidate>, ProviderError> {
        let query = format!("{} {}", track.artist, track.title);
        let resp = self
            .client
            .get(SEARCH_URL)
            .query(&[
                ("qry", query.trim()),
                ("searchButton.x", "0"),
                ("searchButton.y", "0"),
            ])
            .send()
            .map_err(|e| ProviderError::Request(e.to_string()))?;

        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimited {
                retry_after_seconds: None,
            });
        }

        let html_text = resp
            .text()
            .map_err(|e| ProviderError::Parse(e.to_string()))?;
        let document = Html::parse_document(&html_text);

        let link_selector = Selector::parse(".entity_name a").unwrap();
        let links: Vec<_> = document.select(&link_selector).take(TOP_N).collect();

        let mut candidates = Vec::new();
        for link in links {
            let Some(href) = link.value().attr("href") else {
                continue;
            };

            let link_text = link.text().collect::<Vec<_>>().join(" ").trim().to_string();
            let (cand_title, cand_artist) = if let Some((left, right)) = link_text.split_once(" - ")
            {
                (right.trim().to_string(), left.trim().to_string())
            } else if !link_text.is_empty() {
                (link_text, track.artist.clone())
            } else {
                (track.title.clone(), track.artist.clone())
            };

            let detail_url = format!("https://www.megalobiz.com{}", href);
            let detail_resp = match self.client.get(&detail_url).send() {
                Ok(resp) => resp,
                Err(_) => continue,
            };

            if detail_resp.status().as_u16() == 429 {
                return Err(ProviderError::RateLimited {
                    retry_after_seconds: None,
                });
            }

            let Ok(detail_html) = detail_resp.text() else {
                continue;
            };
            let detail_doc = Html::parse_document(&detail_html);

            let lrc_selector = Selector::parse("span.lyrics_details").unwrap();
            if let Some(lrc) = detail_doc.select(&lrc_selector).next() {
                let text = lrc.text().collect::<Vec<_>>().join("");
                if !text.trim().is_empty() {
                    candidates.push(Candidate {
                        title: cand_title,
                        artist: cand_artist,
                        album: None,
                        duration_seconds: None,
                        synced: Some(text),
                        plain: None,
                        provider: ProviderName::Megalobiz,
                    });
                }
            }
        }

        Ok(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_megalobiz_parsing() {
        let html_text = r#"
            <html>
                <body>
                    <div class="entity_name">
                        <a href="/lyrics/something">Artist - Title</a>
                    </div>
                    <div class="entity_name">
                        <a href="/lyrics/second">Another Artist - Second Title</a>
                    </div>
                </body>
            </html>
        "#;
        let document = Html::parse_document(html_text);
        let link_selector = Selector::parse(".entity_name a").unwrap();
        let links: Vec<_> = document.select(&link_selector).collect();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].value().attr("href"), Some("/lyrics/something"));
        assert_eq!(links[1].value().attr("href"), Some("/lyrics/second"));
    }
}
