use std::borrow::Cow;

use crate::collectors::incremental::IncrementalCollector;
use human_errors::ResultExt;
use feed_rs::{model::Entry, parser::parse};
use chrono::{DateTime, Utc};

pub struct XkcdCollector {
    pub feed_url: String,
}

pub struct XkcdItem {
    pub title: String,
    pub url: String,
    pub published: DateTime<Utc>,
    pub image_url: Option<String>,
    pub image_alt: Option<String>,
}

impl IncrementalCollector for XkcdCollector {
    type Item = XkcdItem;
    type Watermark = DateTime<Utc>;

    fn kind(&self) -> &'static str {
        "xkcd"
    }

    fn key(&self) -> Cow<'static, str> {
        Cow::Borrowed("xkcd")
    }

    fn watermark(&self, item: &Self::Item) -> Self::Watermark {
        item.published
    }

    async fn fetch_since(
        &self,
        watermark: Option<Self::Watermark>,
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        let content = reqwest::get(&self.feed_url).await.wrap_err_as_user(
            format!("Failed to fetch RSS feed from URL '{}'.", &self.feed_url),
            &[
                "Check that your network connection is working properly.",
                "Try again later, as the server may be temporarily unavailable.",
            ],
        )?.bytes().await.wrap_err_as_user(
            format!("Failed to read the content of the RSS feed from URL '{}'.", &self.feed_url),
            &[
                "Check that your network connection is working properly.",
                "Try again later, as the server may be temporarily unavailable.",
            ],
        )?;

        parse(&content[..]).wrap_err_as_user(
            format!("Failed to parse RSS feed information from URL '{}'.", &self.feed_url), 
            &[
                "Ensure that the content at the URL is a valid RSS feed.",
                "Try again later, as the server may be temporarily unavailable.",
            ],
        ).map(|feed| feed.entries.into_iter()
            .map(Self::parse_xkcd_entry)
            .filter(|item| watermark.map(|wm| wm < self.watermark(item)).unwrap_or(true)).collect())
    }
}

impl XkcdCollector {
    pub fn new(feed_url: impl ToString) -> Self {
        Self {
            feed_url: feed_url.to_string(),
        }
    }

    fn parse_xkcd_entry(entry: Entry) -> XkcdItem {
        let title = entry.title.as_ref().map(|t| urlencoding::decode(t.content.as_str()).unwrap_or_default().to_string()).unwrap_or_default();
        let url = entry.links.first().map(|l| urlencoding::decode(l.href.as_str()).unwrap_or_default().to_string()).unwrap_or_default();
        let published = entry.published.unwrap_or_else(|| DateTime::UNIX_EPOCH);

        if let Some(content) = entry.summary.as_ref()
            .map(|c| c.content.clone())
            .map(|body| html_escape::decode_html_entities(body.as_str()).to_string())
            .map(|body| scraper::Html::parse_fragment(body.as_ref())) {
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

        let collector: XkcdCollector = XkcdCollector {
            feed_url: mock_server.uri(),
        };

        let items = collector.fetch_since(None).await.unwrap();
        assert_eq!(items.len(), 4, "Expected to fetch 4 RSS items from test data");
        assert_eq!(items[0].title, "Eclipse Clouds");
        assert_eq!(items[0].url, "https://xkcd.com/2915/");
        assert_eq!(items[0].image_url, Some("https://imgs.xkcd.com/comics/eclipse_clouds.png".into()));
        assert_eq!(items[0].image_alt, Some("The rare compound solar-lunar-nephelogical eclipse".into()));
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

        let collector = XkcdCollector {
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
        assert_eq!(items[0].title, "Eclipse Clouds");
    }
}