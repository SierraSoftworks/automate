use std::borrow::Cow;

use crate::collectors::incremental::IncrementalCollector;
use human_errors::ResultExt;
use feed_rs::{model::Entry, parser::parse};
use chrono::{DateTime, Utc};

pub struct YouTubeCollector {
    pub channel_id: String,
    base_url: Cow<'static, str>,
}

impl YouTubeCollector {
    pub fn new(channel_id: &str) -> Self {
        Self {
            channel_id: channel_id.to_string(),
            base_url: "https://www.youtube.com/feeds/videos.xml?channel_id=".into(),
        }
    }

    #[cfg(test)]
    pub fn with_base_url(channel_id: &str, base_url: String) -> Self {
        Self {
            channel_id: channel_id.to_string(),
            base_url: base_url.into(),
        }
    }
}

impl IncrementalCollector for YouTubeCollector {
    type Item = Entry;
    type Watermark = DateTime<Utc>;

    fn key(&self) -> (String, String) {
        ("youtube_collector".to_string(), self.channel_id.clone())
    }

    fn watermark(&self, item: &Self::Item) -> Self::Watermark {
        item.published
            .unwrap_or_else(|| DateTime::UNIX_EPOCH)
    }

    async fn fetch_since(
        &self,
        watermark: Option<Self::Watermark>,
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        let content = reqwest::get(&format!("{}{}", self.base_url, self.channel_id)).await.wrap_err_as_user(
            format!("Failed to fetch YouTube feed for channel '{}'.", &self.channel_id),
            &[
                "Check that the URL is correct and that the server is reachable.",
                "Check that your network connection is working properly.",
            ],
        )?.bytes().await.wrap_err_as_user(
            format!("Failed to read the content of the YouTube feed for channel '{}'.", &self.channel_id),
            &[
                "Check that the URL is correct and that the server is reachable.",
                "Check that your network connection is working properly.",
            ],
        )?;
        parse(&content[..]).wrap_err_as_user(
            format!("Failed to parse YouTube feed information for channel '{}'.", &self.channel_id), 
            &[
                "Ensure that the content at the URL is a valid RSS feed.",
            ],
        ).map(|feed| feed.entries.into_iter()
            .filter(|item| watermark.map(|wm| wm < self.watermark(item)).unwrap_or(true)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{MockServer, Mock, ResponseTemplate};
    use wiremock::matchers::method;

    #[tokio::test]
    async fn test_rss_collector_fetch() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("youtube.atom.xml");

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector = YouTubeCollector::with_base_url("test", format!("{}/", mock_server.uri()));

        let items = collector.fetch_since(None).await.unwrap();
        assert_eq!(items.len(), 15, "Expected to fetch 15 RSS items from test data");
        assert_eq!(items[0].title.as_ref().unwrap().content, "Remember to always dispose of chemicals properly 🌎");
        assert_eq!(items[3].title.as_ref().unwrap().content, "Would you recommend aftermarket bars or not?");
    }

    #[tokio::test]
    async fn test_rss_collector_fetch_with_watermark() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("youtube.atom.xml");

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector = YouTubeCollector::with_base_url("test", format!("{}/", mock_server.uri()));

        // Watermark set to filter items on or before April 1, 2024
        let watermark = Some(
            DateTime::parse_from_rfc3339("2024-04-04T12:00:45+00:00")
                .unwrap()
                .with_timezone(&Utc)
        );
        let items = collector.fetch_since(watermark).await.unwrap();
        
        assert_eq!(items.len(), 1, "Expected only items after watermark");
        assert_eq!(items[0].title.as_ref().unwrap().content, "Remember to always dispose of chemicals properly 🌎");
    }
}