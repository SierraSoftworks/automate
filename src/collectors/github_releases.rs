use chrono::Utc;
use human_errors::ResultExt;
use serde::Deserialize;
use tracing_batteries::prelude::*;

use crate::filter::Filterable;

use super::{Collector, IncrementalCollector};

pub struct GitHubReleasesCollector {
    api_url: String,
    repo: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct GitHubReleaseItem {
    pub tag_name: String,
    pub target_commitish: String,

    pub name: String,
    pub body: Option<String>,

    pub draft: bool,
    pub prerelease: bool,

    pub created_at: chrono::DateTime<chrono::Utc>,
    pub published_at: chrono::DateTime<chrono::Utc>,

    pub html_url: String,
}

impl Filterable for GitHubReleaseItem {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "tag" => self.tag_name.clone().into(),
            "name" => self.name.clone().into(),
            "published" => self.published_at.to_rfc3339().into(),
            "link" => self.html_url.clone().into(),
            "draft" => self.draft.into(),
            "prerelease" => self.prerelease.into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

impl GitHubReleasesCollector {
    pub fn new(repo: impl ToString) -> Self {
        Self {
            api_url: "https://api.github.com".into(),
            repo: repo.to_string(),
        }
    }

    #[cfg(test)]
    pub fn new_with_url(url: impl ToString, repo: impl ToString) -> Self {
        Self {
            api_url: url.to_string(),
            repo: repo.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl Collector for GitHubReleasesCollector {
    type Item = GitHubReleaseItem;

    #[instrument("collectors.github_releases.list", skip(self, services), err(Display))]
    async fn list(
        &self,
        services: &(impl crate::services::Services + Send + Sync + 'static),
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        self.fetch(services).await
    }
}

impl IncrementalCollector for GitHubReleasesCollector {
    type Watermark = chrono::DateTime<chrono::Utc>;

    fn kind(&self) -> &'static str {
        "github_releases"
    }

    fn key(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(self.api_url.clone())
    }

    #[instrument(
        "collectors.github_releases.fetch_since",
        skip(self, services),
        err(Display)
    )]
    async fn fetch_since(
        &self,
        watermark: Option<Self::Watermark>,
        services: &impl crate::services::Services,
    ) -> Result<(Vec<Self::Item>, Self::Watermark), human_errors::Error> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("X-GitHub-Api-Version", "2022-11-28".parse().unwrap());

        if let Some(api_key) = services.config().connections.github.api_key.as_ref() {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Bearer {}", api_key))
                    .or_system_err(&["Report the issue to the development team on GitHub."])?,
            );
        }

        let client = reqwest::Client::builder()
            .user_agent("SierraSoftworks/automate-rs")
            .default_headers(headers)
            .build()
            .or_system_err(&["Report the issue to the development team on GitHub."])?;

        let response = client.get(format!("{}/repos/{}/releases", self.api_url, self.repo))
            .send().await.wrap_user_err("We were unable to fetch GitHub releases from GitHub.", &[
                "Make sure that your network connection is working properly.",
                "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
            ])?;

        match response.status() {
            reqwest::StatusCode::OK => {}
            reqwest::StatusCode::NOT_FOUND => {
                return Err(human_errors::user(
                    "The specified GitHub repository was not found when trying to fetch releases.",
                    &[
                        "Ensure that the repository exists and that the URL is correct.",
                        "If the repository is private, ensure that your API key has access to it.",
                    ],
                ));
            }
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                return Err(human_errors::user(
                    "Authorization failed when trying to fetch GitHub releases.",
                    &[
                        "Ensure that your API key is correct and has the necessary permissions to access the repository releases.",
                        "If you recently changed your API key, make sure to update it in your configuration.",
                    ],
                ));
            }
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                return Err(human_errors::user(
                    "Rate limit exceeded when trying to fetch GitHub releases.",
                    &[
                        "Wait for a while before making more requests to GitHub's API.",
                        "Consider using an authenticated API key to increase your rate limit.",
                    ],
                ));
            }
            status => {
                return Err(human_errors::user(
                    format!(
                        "Failed to fetch GitHub releases. Received unexpected status code: {}",
                        status
                    ),
                    &[
                        "Make sure that your network connection is working properly.",
                        "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
                    ],
                ));
            }
        }

        let releases: Vec<GitHubReleaseItem> = response.json().await.wrap_user_err(
            format!(
                "Failed to read the content of the GitHub Releases from URL '{}'.",
                &self.api_url
            ),
            &[
                "Check that the URL is correct and that the server is reachable.",
                "Check that your network connection is working properly.",
            ],
        )?;

        let latest_release = releases
            .iter()
            .map(|item| item.published_at)
            .max()
            .unwrap_or(Utc::now());
        if let Some(watermark) = watermark {
            Ok((
                releases
                    .into_iter()
                    .filter(|item| item.published_at > watermark)
                    .collect(),
                latest_release,
            ))
        } else {
            Ok((releases, latest_release))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::KeyValueStore;
    use crate::services::Services;
    use crate::testing::mock_services;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_github_releases_fetch_since_no_watermark() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("github_releases.json");

        Mock::given(method("GET"))
            .and(path("/repos/example/repo/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector = GitHubReleasesCollector::new_with_url(mock_server.uri(), "example/repo");
        let services = mock_services().await.unwrap();

        let (items, watermark) = collector.fetch_since(None, &services).await.unwrap();

        assert_eq!(
            items.len(),
            4,
            "Expected to fetch 4 releases from test data"
        );
        assert_eq!(items[0].tag_name, "v1.2.0");
        assert_eq!(items[0].name, "Release 1.2.0");
        assert_eq!(
            items[0].html_url,
            "https://github.com/example/repo/releases/tag/v1.2.0"
        );
        assert!(!items[0].draft);
        assert!(!items[0].prerelease);

        assert_eq!(items[3].tag_name, "v1.0.0");
        assert_eq!(items[3].name, "Initial Release");

        // Watermark should be set to the latest published_at date
        assert_eq!(
            watermark,
            chrono::DateTime::parse_from_rfc3339("2024-04-15T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[tokio::test]
    async fn test_github_releases_fetch_since_with_watermark() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("github_releases.json");

        Mock::given(method("GET"))
            .and(path("/repos/example/repo/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector = GitHubReleasesCollector::new_with_url(mock_server.uri(), "example/repo");
        let services = mock_services().await.unwrap();

        // Set watermark to filter releases on or before March 1, 2024
        let watermark = Some(
            chrono::DateTime::parse_from_rfc3339("2024-03-01T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        let (items, _) = collector.fetch_since(watermark, &services).await.unwrap();

        assert_eq!(items.len(), 1, "Expected only releases after watermark");
        assert_eq!(items[0].tag_name, "v1.2.0");
        assert_eq!(items[0].name, "Release 1.2.0");
    }

    #[tokio::test]
    async fn test_github_releases_404_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/nonexistent/repo/releases"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let collector =
            GitHubReleasesCollector::new_with_url(mock_server.uri(), "nonexistent/repo");
        let services = mock_services().await.unwrap();

        let result = collector.fetch_since(None, &services).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_github_releases_401_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/private/repo/releases"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&mock_server)
            .await;

        let collector = GitHubReleasesCollector::new_with_url(mock_server.uri(), "private/repo");
        let services = mock_services().await.unwrap();

        let result = collector.fetch_since(None, &services).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Authorization failed"));
    }

    #[tokio::test]
    async fn test_github_releases_429_rate_limit() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/example/repo/releases"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let collector = GitHubReleasesCollector::new_with_url(mock_server.uri(), "example/repo");
        let services = mock_services().await.unwrap();

        let result = collector.fetch_since(None, &services).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Rate limit exceeded"));
    }

    #[tokio::test]
    async fn test_github_releases_list_collector_trait() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("github_releases.json");

        Mock::given(method("GET"))
            .and(path("/repos/example/repo/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector = GitHubReleasesCollector::new_with_url(mock_server.uri(), "example/repo");
        let services = mock_services().await.unwrap();

        let items = collector.list(&services).await.unwrap();
        assert_eq!(items.len(), 4, "Expected to fetch 4 releases via list()");
    }

    #[tokio::test]
    async fn test_github_releases_incremental_with_stored_watermark() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("github_releases.json");

        Mock::given(method("GET"))
            .and(path("/repos/example/repo/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_string(test_data))
            .mount(&mock_server)
            .await;

        let collector = GitHubReleasesCollector::new_with_url(mock_server.uri(), "example/repo");
        let services = mock_services().await.unwrap();

        // Store a watermark in the KV store
        services
            .kv()
            .set(
                collector.partition(None),
                collector.key(),
                chrono::DateTime::parse_from_rfc3339("2024-03-01T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            )
            .await
            .unwrap();

        // List should use the stored watermark
        let items = collector.list(&services).await.unwrap();
        assert_eq!(
            items.len(),
            1,
            "Expected only releases after stored watermark"
        );
        assert_eq!(items[0].tag_name, "v1.2.0");
    }

    #[test]
    fn test_github_release_item_filterable() {
        let item = GitHubReleaseItem {
            tag_name: "v1.0.0".to_string(),
            target_commitish: "main".to_string(),
            name: "Test Release".to_string(),
            body: Some("Test body".to_string()),
            draft: false,
            prerelease: true,
            created_at: chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            published_at: chrono::DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            html_url: "https://github.com/example/repo/releases/tag/v1.0.0".to_string(),
        };

        assert_eq!(
            item.get("tag"),
            crate::filter::FilterValue::String("v1.0.0".to_string())
        );
        assert_eq!(
            item.get("name"),
            crate::filter::FilterValue::String("Test Release".to_string())
        );
        assert_eq!(
            item.get("link"),
            crate::filter::FilterValue::String(
                "https://github.com/example/repo/releases/tag/v1.0.0".to_string()
            )
        );
        assert_eq!(item.get("draft"), crate::filter::FilterValue::Bool(false));
        assert_eq!(
            item.get("prerelease"),
            crate::filter::FilterValue::Bool(true)
        );
        assert_eq!(item.get("unknown"), crate::filter::FilterValue::Null);
    }
}
