use crate::prelude::*;
use std::borrow::Cow;

use crate::collectors::{Collector, incremental::IncrementalCollector};
use chrono::{DateTime, Utc};
use feed_rs::{model::Entry, parser::parse};

/// The watermark persisted between RSS collector runs.
///
/// It tracks the publication date of the most recent entry we have seen (used
/// to filter out entries we have already processed) along with the optional
/// HTTP cache validators returned by the server. The `ETag` and
/// `Last-Modified` values let us issue a conditional request so we can skip
/// downloading the feed entirely when nothing has changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RssWatermark {
    pub published: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// The raw `Last-Modified` header value from the server, stored verbatim so
    /// we can echo it back in `If-Modified-Since` without any reformatting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

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

    #[instrument("collectors.rss.list", skip(self, services), err(Display))]
    async fn list(
        &self,
        services: &(impl crate::services::Services + Send + Sync + 'static),
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        self.fetch(services).await
    }
}

impl IncrementalCollector for RssCollector {
    type Watermark = RssWatermark;

    fn partition(&self) -> &'static str {
        "rss/feed"
    }

    fn key(&self) -> Cow<'static, str> {
        Cow::Owned(self.feed_url.clone())
    }

    #[instrument("collectors.rss.fetch_since", skip(self, services), err(Display))]
    async fn fetch_since(
        &self,
        watermark: Option<Self::Watermark>,
        services: &impl crate::services::Services,
    ) -> Result<(Vec<Self::Item>, Self::Watermark), human_errors::Error> {
        let previous_published = watermark.as_ref().map(|wm| wm.published);
        let previous_etag = watermark.as_ref().and_then(|wm| wm.etag.clone());
        let previous_last_modified = watermark.as_ref().and_then(|wm| wm.last_modified.clone());

        let mut request = services.http_client().get(&self.feed_url);
        if let Some(etag) = &previous_etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        if let Some(last_modified) = &previous_last_modified {
            request = request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
        }

        let response = request.send().await.wrap_user_err(
            format!("Failed to fetch RSS feed from URL '{}'.", &self.feed_url),
            &[
                "Check that the URL is correct and that the server is reachable.",
                "Check that your network connection is working properly.",
            ],
        )?;

        // The server told us nothing has changed since our last request, so we
        // can skip downloading and parsing the feed entirely and preserve the
        // existing watermark.
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok((
                Vec::new(),
                RssWatermark {
                    published: previous_published.unwrap_or_else(Utc::now),
                    etag: previous_etag,
                    last_modified: previous_last_modified,
                },
            ));
        }

        let response = response.error_for_status().wrap_user_err(
            format!(
                "We received an unexpected error response from URL '{}'.",
                &self.feed_url
            ),
            &["Check that the URL is correct and that the server is reachable."],
        )?;

        let new_etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());

        let new_last_modified = response
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());

        let content = response.bytes().await.wrap_user_err(
            format!(
                "Failed to read the content of the RSS feed from URL '{}'.",
                &self.feed_url
            ),
            &[
                "Check that the URL is correct and that the server is reachable.",
                "Check that your network connection is working properly.",
            ],
        )?;

        let items: Vec<Entry> = parse(&content[..])
            .wrap_user_err(
                format!(
                    "Failed to parse RSS feed information from URL '{}'.",
                    self.feed_url
                ),
                &["Ensure that the content at the URL is a valid RSS feed."],
            )
            .map(|feed| {
                feed.entries
                    .into_iter()
                    .filter(|item| {
                        previous_published
                            .map(|wm| wm < item.published.unwrap_or(DateTime::UNIX_EPOCH))
                            .unwrap_or(true)
                    })
                    .collect()
            })?;

        let new_published = items
            .iter()
            .filter_map(|item| item.published)
            .max()
            .or(previous_published)
            .unwrap_or_else(Utc::now);

        Ok((
            items,
            RssWatermark {
                published: new_published,
                etag: new_etag,
                last_modified: new_last_modified,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
        let services = crate::testing::mock_services().await.unwrap();

        let (items, _) = collector.fetch_since(None, &services).await.unwrap();
        assert_eq!(
            items.len(),
            4,
            "Expected to fetch 4 RSS items from test data"
        );
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
        let services = crate::testing::mock_services().await.unwrap();

        // Watermark set to filter items on or before April 1, 2024
        let watermark = Some(RssWatermark {
            published: DateTime::parse_from_rfc2822("Mon, 01 Apr 2024 04:00:00 -0000")
                .unwrap()
                .with_timezone(&Utc),
            etag: None,
            last_modified: None,
        });
        let (items, _) = collector.fetch_since(watermark, &services).await.unwrap();

        assert_eq!(items.len(), 1, "Expected only items after watermark");
        assert_eq!(items[0].title.as_ref().unwrap().content, "Eclipse Clouds");
    }

    #[tokio::test]
    async fn test_rss_collector_captures_etag() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("xkcd.rss.xml");

        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"abc123\"")
                    .set_body_string(test_data),
            )
            .mount(&mock_server)
            .await;

        let collector = RssCollector {
            feed_url: mock_server.uri(),
        };
        let services = crate::testing::mock_services().await.unwrap();

        let (items, watermark) = collector.fetch_since(None, &services).await.unwrap();
        assert_eq!(
            items.len(),
            4,
            "Expected to fetch 4 RSS items from test data"
        );
        assert_eq!(
            watermark.etag.as_deref(),
            Some("\"abc123\""),
            "Expected the ETag from the response to be captured in the watermark"
        );
    }

    #[tokio::test]
    async fn test_rss_collector_sends_if_none_match_and_handles_not_modified() {
        let mock_server = MockServer::start().await;

        // The collector should send the previously captured ETag via the
        // If-None-Match header; respond with 304 to simulate an unchanged feed.
        Mock::given(method("GET"))
            .and(header("If-None-Match", "\"abc123\""))
            .respond_with(ResponseTemplate::new(304))
            .mount(&mock_server)
            .await;

        let collector = RssCollector {
            feed_url: mock_server.uri(),
        };
        let services = crate::testing::mock_services().await.unwrap();

        let previous = DateTime::parse_from_rfc2822("Mon, 01 Apr 2024 04:00:00 -0000")
            .unwrap()
            .with_timezone(&Utc);
        let watermark = Some(RssWatermark {
            published: previous,
            etag: Some("\"abc123\"".to_string()),
            last_modified: None,
        });

        let (items, new_watermark) = collector.fetch_since(watermark, &services).await.unwrap();

        assert!(
            items.is_empty(),
            "Expected no items when the feed has not been modified"
        );
        assert_eq!(
            new_watermark.published, previous,
            "Expected the published watermark to be preserved on a 304 response"
        );
        assert_eq!(
            new_watermark.etag.as_deref(),
            Some("\"abc123\""),
            "Expected the ETag to be preserved on a 304 response"
        );
    }

    #[tokio::test]
    async fn test_rss_collector_captures_last_modified() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("xkcd.rss.xml");

        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Last-Modified", "Mon, 01 Apr 2024 04:00:00 GMT")
                    .set_body_string(test_data),
            )
            .mount(&mock_server)
            .await;

        let collector = RssCollector {
            feed_url: mock_server.uri(),
        };
        let services = crate::testing::mock_services().await.unwrap();

        let (items, watermark) = collector.fetch_since(None, &services).await.unwrap();
        assert_eq!(
            items.len(),
            4,
            "Expected to fetch 4 RSS items from test data"
        );
        assert_eq!(
            watermark.last_modified.as_deref(),
            Some("Mon, 01 Apr 2024 04:00:00 GMT"),
            "Expected the Last-Modified header to be captured in the watermark"
        );
    }

    #[tokio::test]
    async fn test_rss_collector_sends_if_modified_since_and_handles_not_modified() {
        let mock_server = MockServer::start().await;

        // The collector should send the previously captured Last-Modified value
        // via the If-Modified-Since header; respond with 304 to simulate an
        // unchanged feed. We match the header with a custom predicate because
        // wiremock's `header` matcher splits values on commas, which are present
        // in HTTP date strings.
        Mock::given(method("GET"))
            .and(|req: &wiremock::Request| {
                req.headers
                    .get("If-Modified-Since")
                    .and_then(|value| value.to_str().ok())
                    == Some("Mon, 01 Apr 2024 04:00:00 GMT")
            })
            .respond_with(ResponseTemplate::new(304))
            .mount(&mock_server)
            .await;

        let collector = RssCollector {
            feed_url: mock_server.uri(),
        };
        let services = crate::testing::mock_services().await.unwrap();

        let previous = DateTime::parse_from_rfc2822("Mon, 01 Apr 2024 04:00:00 -0000")
            .unwrap()
            .with_timezone(&Utc);
        let watermark = Some(RssWatermark {
            published: previous,
            etag: None,
            last_modified: Some("Mon, 01 Apr 2024 04:00:00 GMT".to_string()),
        });

        let (items, new_watermark) = collector.fetch_since(watermark, &services).await.unwrap();

        assert!(
            items.is_empty(),
            "Expected no items when the feed has not been modified"
        );
        assert_eq!(
            new_watermark.published, previous,
            "Expected the published watermark to be preserved on a 304 response"
        );
        assert_eq!(
            new_watermark.last_modified.as_deref(),
            Some("Mon, 01 Apr 2024 04:00:00 GMT"),
            "Expected the Last-Modified value to be preserved on a 304 response"
        );
    }
}
