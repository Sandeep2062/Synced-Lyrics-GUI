use super::{LyricsProvider, ProviderError, ProviderLyrics, ProviderName};
use crate::model::Track;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::time::Duration;

const SEARCH_URL: &str = "https://www.megalobiz.com/search/all";

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
}

impl LyricsProvider for MegalobizProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Megalobiz
    }

    fn search(&self, track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
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
        let link_element = document.select(&link_selector).next();

        if let Some(link) = link_element {
            if let Some(href) = link.value().attr("href") {
                let detail_url = format!("https://www.megalobiz.com{}", href);
                let detail_resp = self
                    .client
                    .get(&detail_url)
                    .send()
                    .map_err(|e| ProviderError::Request(e.to_string()))?;

                let detail_html = detail_resp
                    .text()
                    .map_err(|e| ProviderError::Parse(e.to_string()))?;
                let detail_doc = Html::parse_document(&detail_html);

                let lrc_selector = Selector::parse("span.lyrics_details").unwrap();
                let lrc_element = detail_doc.select(&lrc_selector).next();

                if let Some(lrc) = lrc_element {
                    let text = lrc.text().collect::<Vec<_>>().join("");
                    return Ok(Some(ProviderLyrics {
                        synced: Some(text),
                        plain: None,
                        confidence: 0.7,
                        provider: self.name(),
                    }));
                }
            }
        }
        Ok(None)
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
                        <a href="/lyrics/something">Link</a>
                    </div>
                </body>
            </html>
        "#;
        let document = Html::parse_document(html_text);
        let link_selector = Selector::parse(".entity_name a").unwrap();
        let link_element = document.select(&link_selector).next();
        assert!(link_element.is_some());
        assert_eq!(
            link_element.unwrap().value().attr("href"),
            Some("/lyrics/something")
        );
    }
}
