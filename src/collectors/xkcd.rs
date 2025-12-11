use crate::{
    collectors::{Collector, RssCollector, incremental::IncrementalCollector},
    filter::Filterable,
};
use chrono::{DateTime, Utc};
use feed_rs::model::Entry;
use tracing_batteries::prelude::*;

pub struct XkcdCollector(RssCollector);

#[allow(dead_code)]
pub struct XkcdItem {
    pub title: String,
    pub url: String,
    pub published: DateTime<Utc>,
    pub image_url: Option<String>,
    pub image_alt: Option<String>,
}

impl Filterable for XkcdItem {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "title" => self.title.clone().into(),
            "url" => self.url.clone().into(),
            "has_image" => self.image_url.is_some().into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[async_trait::async_trait]
impl Collector for XkcdCollector {
    type Item = XkcdItem;

    #[instrument("collectors.xkcd.list", skip(self, services), err(Display))]
    async fn list(
        &self,
        services: &(impl crate::services::Services + Send + Sync + 'static),
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        let items = self.0.fetch(services).await?;

        let xkcd_items = items
            .into_iter()
            .map(XkcdCollector::parse_xkcd_entry)
            .collect();

        Ok(xkcd_items)
    }
}

impl XkcdCollector {
    pub fn new() -> Self {
        Self(RssCollector::new("https://xkcd.com/rss.xml"))
    }

    #[cfg(test)]
    pub fn new_with_feed(feed_url: impl ToString) -> Self {
        Self(RssCollector::new(feed_url))
    }

    fn parse_xkcd_entry(entry: Entry) -> XkcdItem {
        let title = entry
            .title
            .as_ref()
            .map(|t| {
                urlencoding::decode(t.content.as_str())
                    .unwrap_or_default()
                    .to_string()
            })
            .unwrap_or_default();
        let url = entry
            .links
            .first()
            .map(|l| {
                urlencoding::decode(l.href.as_str())
                    .unwrap_or_default()
                    .to_string()
            })
            .unwrap_or_default();
        let published = entry.published.unwrap_or_else(|| DateTime::UNIX_EPOCH);

        if let Some(content) = entry
            .summary
            .as_ref()
            .map(|c| c.content.clone())
            .map(|body| html_escape::decode_html_entities(body.as_str()).to_string())
            .map(|body| scraper::Html::parse_fragment(body.as_ref()))
        {
            let img_selector = scraper::Selector::parse("img").unwrap();

            if let Some(img_element) = content.select(&img_selector).next() {
                let src = img_element.value().attr("src").map(|s| s.to_string());
                let alt = img_element.value().attr("alt").map(|s| s.to_string());
                return XkcdItem {
                    title,
                    url,
                    published,
                    image_url: src,
                    image_alt: alt,
                };
            }
        }

        XkcdItem {
            title,
            url,
            published,
            image_url: None,
            image_alt: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::db::KeyValueStore;
    use crate::services::Services;
    use crate::testing::mock_services;

    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_rss_collector_fetch() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("xkcd.rss.xml");

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector: XkcdCollector = XkcdCollector::new_with_feed(mock_server.uri());
        let services = mock_services().await.unwrap();

        let items = collector.list(&services).await.unwrap();
        assert_eq!(
            items.len(),
            4,
            "Expected to fetch 4 RSS items from test data"
        );
        assert_eq!(items[0].title, "Eclipse Clouds");
        assert_eq!(items[0].url, "https://xkcd.com/2915/");
        assert_eq!(
            items[0].image_url,
            Some("https://imgs.xkcd.com/comics/eclipse_clouds.png".into())
        );
        assert_eq!(
            items[0].image_alt,
            Some("The rare compound solar-lunar-nephelogical eclipse".into())
        );
        assert_eq!(items[3].title, "Cursive Letters");
    }

    #[tokio::test]
    async fn test_rss_collector_fetch_with_watermark() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("xkcd.rss.xml");

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector = XkcdCollector::new_with_feed(mock_server.uri());
        let services = mock_services().await.unwrap();

        // Store watermark to filter items on or before April 1, 2024
        services
            .kv()
            .set(
                collector.0.partition(None),
                collector.0.key(),
                DateTime::parse_from_rfc2822("Mon, 01 Apr 2024 04:00:00 -0000")
                    .unwrap()
                    .with_timezone(&Utc),
            )
            .await
            .unwrap();

        let items = collector.list(&services).await.unwrap();

        assert_eq!(items.len(), 1, "Expected only items after watermark");
        assert_eq!(items[0].title, "Eclipse Clouds");
    }
}
