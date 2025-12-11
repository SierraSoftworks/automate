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
#[derive(Deserialize)]
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
                    .map_err_as_system(&["Report the issue to the development team on GitHub."])?,
            );
        }

        let client = reqwest::Client::builder()
            .user_agent("SierraSoftworks/automate-rs")
            .default_headers(headers)
            .build()
            .map_err_as_system(&["Report the issue to the development team on GitHub."])?;

        let response = client.get(format!("{}/repos/{}/releases", self.api_url, self.repo))
            .send().await.wrap_err_as_user("We were unable to fetch GitHub releases from GitHub.", &[
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

        let releases: Vec<GitHubReleaseItem> = response.json().await.wrap_err_as_user(
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

// TODO: Add tests for the GitHubReleasesCollector using wiremock to mock out the GitHub API and test data stored in the tests/data/ directory.