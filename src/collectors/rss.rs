use std::borrow::Cow;

use crate::collectors::{Collector, incremental::IncrementalCollector};
use human_errors::ResultExt;
use feed_rs::{model::Entry, parser::parse};
use chrono::{DateTime, Utc};

pub struct RssCollector {
    pub feed_url: String,
}

impl RssCollector {
    pub fn new(feed_url: impl ToString) -> Self {
        Self {
            feed_url: feed_url.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl Collector for RssCollector {
    type Item = Entry;

    async fn list(&self, services: &(impl crate::services::Services + Send + Sync + 'static)) -> Result<Vec<Self::Item>, human_errors::Error> {
        self.fetch(services).await
    }
}

impl IncrementalCollector for RssCollector {
    type Watermark = DateTime<Utc>;

    fn kind(&self) -> &'static str {
        "rss"
    }

    fn key(&self) -> Cow<'static, str> {
        Cow::Owned(self.feed_url.clone())
    }

    fn watermark(&self, item: &Self::Item) -> Self::Watermark {
        item.published.unwrap_or_else(|| DateTime::UNIX_EPOCH)
    }

    async fn fetch_since(
        &self,
        watermark: Option<Self::Watermark>,
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        let content = reqwest::get(&self.feed_url).await.wrap_err_as_user(
            format!("Failed to fetch RSS feed from URL '{}'.", &self.feed_url),
            &[
                "Check that the URL is correct and that the server is reachable.",
                "Check that your network connection is working properly.",
            ],
        )?.bytes().await.wrap_err_as_user(
            format!("Failed to read the content of the RSS feed from URL '{}'.", &self.feed_url),
            &[
                "Check that the URL is correct and that the server is reachable.",
                "Check that your network connection is working properly.",
            ],
        )?;

        parse(&content[..]).wrap_err_as_user(
            format!("Failed to parse RSS feed information from URL '{}'.", self.feed_url), 
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
        let test_data = crate::testing::get_test_file_contents("xkcd.rss.xml");

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector = RssCollector {
            feed_url: mock_server.uri(),
        };

        let items = collector.fetch_since(None).await.unwrap();
        assert_eq!(items.len(), 4, "Expected to fetch 4 RSS items from test data");
        assert_eq!(items[0].title.as_ref().unwrap().content, "Eclipse Clouds");
        assert_eq!(items[3].title.as_ref().unwrap().content, "Cursive Letters");
    }

    #[tokio::test]
    async fn test_rss_collector_fetch_with_watermark() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("xkcd.rss.xml");

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector = RssCollector {
            feed_url: mock_server.uri(),
        };

        // Watermark set to filter items on or before April 1, 2024
        let watermark = Some(
            DateTime::parse_from_rfc2822("Mon, 01 Apr 2024 04:00:00 -0000")
                .unwrap()
                .with_timezone(&Utc)
        );
        let items = collector.fetch_since(watermark).await.unwrap();
        
        assert_eq!(items.len(), 1, "Expected only items after watermark");
        assert_eq!(items[0].title.as_ref().unwrap().content, "Eclipse Clouds");
    }
}