use crate::{collectors::{Collector, RssCollector, incremental::IncrementalCollector}};
use feed_rs::{model::Entry};
use chrono::{DateTime, Utc};

pub struct YouTubeCollector(RssCollector);

#[allow(dead_code)]
pub struct YouTubeItem {
    pub channel: String,
    pub title: String,
    pub link: String,
    pub published: DateTime<Utc>,
}

impl YouTubeCollector {
    pub fn new(channel_id: impl ToString) -> Self {
        Self(RssCollector::new(
            format!("https://www.youtube.com/feeds/videos.xml?channel_id={}", channel_id.to_string())
        ))
    }

    #[cfg(test)]
    pub fn new_with_feed(feed_url: impl ToString) -> Self {
        Self(RssCollector::new(feed_url))
    }
}

#[async_trait::async_trait]
impl Collector for YouTubeCollector {
    type Item = YouTubeItem;

    async fn list(&self, services: &(impl crate::services::Services + Send + Sync + 'static)) -> Result<Vec<Self::Item>, human_errors::Error> {
        let items = self.0.fetch(services).await?;

        let youtube_items = items.iter()
            .map(|entry| parse_youtube_entry(entry))
            .collect();

        Ok(youtube_items)
    }
}

fn parse_youtube_entry(entry: &Entry) -> YouTubeItem {
    let title = entry.title.as_ref().map(|t| t.content.to_string()).unwrap_or_default();
    let link = entry.links.first().map(|l| l.href.to_string()).unwrap_or_default();
    let published = entry.published.unwrap_or_else(|| DateTime::UNIX_EPOCH);
    let channel = entry.authors.first().map(|a| a.name.to_string()).unwrap_or_default();

    YouTubeItem {
        channel,
        title,
        link,
        published,
    }
}

#[cfg(test)]
mod tests {
    use crate::services::Services;
    use crate::db::KeyValueStore;

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

        let collector = YouTubeCollector::new_with_feed(mock_server.uri());
        let services = crate::testing::mock_services().await.unwrap();

        let items = collector.list(&services).await.unwrap();
        assert_eq!(items.len(), 15, "Expected to fetch 15 RSS items from test data");
        assert_eq!(items[0].title, "Remember to always dispose of chemicals properly 🌎");
        assert_eq!(items[3].title, "Would you recommend aftermarket bars or not?");
    }

    #[tokio::test]
    async fn test_rss_collector_fetch_with_watermark() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("youtube.atom.xml");

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector = YouTubeCollector::new_with_feed(mock_server.uri());
        let services = crate::testing::mock_services().await.unwrap();

        // Set watermark to filter items on or before April 4, 2024
        services.kv().set(
            collector.0.partition(None),
            collector.0.key(),
            DateTime::parse_from_rfc3339("2024-04-04T12:00:45+00:00")
                .unwrap()
                .with_timezone(&Utc)
        ).await.unwrap();

        let items = collector.list(&services).await.unwrap();
        
        assert_eq!(items.len(), 1, "Expected only items after watermark");
        assert_eq!(items[0].title, "Remember to always dispose of chemicals properly 🌎");
    }
}