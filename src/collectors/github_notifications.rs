use std::fmt::Display;

use human_errors::ResultExt;
use serde::{Deserialize, Serialize};
use tracing_batteries::prelude::*;

use crate::filter::Filterable;

use super::{Collector, IncrementalCollector};

pub struct GitHubNotificationsCollector {
    api_url: String,
}

impl GitHubNotificationsCollector {
    pub fn new() -> Self {
        Self {
            api_url: "https://api.github.com".into(),
        }
    }

    #[cfg(test)]
    pub fn new_with_url(url: impl ToString) -> Self {
        Self {
            api_url: url.to_string(),
        }
    }

    #[instrument(
        "collectors.github_notifications.get_subject_state",
        skip(self, subject, services)
    )]
    pub async fn get_subject(
        &self,
        subject: &GitHubNotificationsSubject,
        services: &(impl crate::services::Services + Send + Sync + 'static),
    ) -> Result<Option<GitHubSubjectInformation>, human_errors::Error> {
        if let Some(url) = &subject.url {
            let client = self.get_client(services)?;

            let response = client.get(url)
                .send().await.wrap_user_err("We were unable to fetch GitHub notification subject state from GitHub.", &[
                    "Make sure that your network connection is working properly.",
                    "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
                ])?;

            match response.status() {
                reqwest::StatusCode::OK => {}
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                    return Err(human_errors::user(
                        "Authorization failed when trying to fetch GitHub notification subject state.",
                        &[
                            "Ensure that your API key is correct and has the necessary permissions to access your notifications.",
                            "If you recently changed your API key, make sure to update it in your configuration.",
                        ],
                    ));
                }
                reqwest::StatusCode::TOO_MANY_REQUESTS => {
                    return Err(human_errors::user(
                        "Rate limit exceeded when trying to fetch GitHub notification subject state.",
                        &["Wait for a while before making more requests to GitHub's API."],
                    ));
                }
                status => {
                    return Err(human_errors::user(
                        format!(
                            "Failed to fetch GitHub notification subject state. Received unexpected status code: {}",
                            status
                        ),
                        &[
                            "Make sure that your network connection is working properly.",
                            "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
                        ],
                    ));
                }
            }

            let issue: GitHubSubjectInformation = response.json().await.wrap_user_err(
                format!(
                    "Failed to read the content of the GitHub notification subject from URL '{}'.",
                    url
                ),
                &[
                    "Check that the URL is correct and that the server is reachable.",
                    "Check that your network connection is working properly.",
                ],
            )?;

            Ok(Some(issue))
        } else {
            Ok(None)
        }
    }

    #[instrument(
        "collectors.github_notifications.mark_as_done",
        skip(self, thread_id, services)
    )]
    pub async fn mark_as_done(
        &self,
        thread_id: &str,
        services: &(impl crate::services::Services + Send + Sync + 'static),
    ) -> Result<(), human_errors::Error> {
        let client = self.get_client(services)?;

        let response = client
            .delete(format!("{}/notifications/threads/{}", self.api_url, thread_id))
            .send().await.wrap_user_err("We were unable to mark the GitHub notification as read.", &[
                "Make sure that your network connection is working properly.",
                "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
            ])?;

        match response.status() {
            reqwest::StatusCode::NO_CONTENT => Ok(()),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                Err(human_errors::user(
                    "Authorization failed when trying to mark GitHub notification as read.",
                    &[
                        "Ensure that your API key is correct and has the necessary permissions to access your notifications.",
                        "If you recently changed your API key, make sure to update it in your configuration.",
                    ],
                ))
            }
            status => Err(human_errors::user(
                format!(
                    "Failed to mark GitHub notification as read. Received unexpected status code: {}",
                    status
                ),
                &[
                    "Make sure that your network connection is working properly.",
                    "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
                ],
            )),
        }
    }

    fn get_client(
        &self,
        services: &impl crate::services::Services,
    ) -> Result<reqwest::Client, human_errors::Error> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("X-GitHub-Api-Version", "2022-11-28".parse().unwrap());
        headers.insert("Accept", "application/vnd.github+json".parse().unwrap());

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

        Ok(client)
    }
}

#[async_trait::async_trait]
impl Collector for GitHubNotificationsCollector {
    type Item = GitHubNotificationsItem;

    #[instrument(
        "collectors.github_notifications.list",
        skip(self, services),
        err(Display)
    )]
    async fn list(
        &self,
        services: &(impl crate::services::Services + Send + Sync + 'static),
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        self.fetch(services).await
    }
}

impl IncrementalCollector for GitHubNotificationsCollector {
    type Watermark = String;

    fn kind(&self) -> &'static str {
        "github_notifications"
    }

    fn key(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(self.api_url.clone())
    }

    #[instrument(
        "collectors.github_notifications.fetch_since",
        skip(self, services),
        err(Display)
    )]
    async fn fetch_since(
        &self,
        watermark: Option<Self::Watermark>,
        services: &impl crate::services::Services,
    ) -> Result<(Vec<Self::Item>, Self::Watermark), human_errors::Error> {
        let client = self.get_client(services)?;

        let response = client.get(format!("{}/notifications", self.api_url))
            .header("If-Modified-Since", watermark.as_deref().unwrap_or("Thu, 01 Jan 1970 00:00:00 GMT"))
            .send().await.wrap_user_err("We were unable to fetch GitHub notifications from GitHub.", &[
                "Make sure that your network connection is working properly.",
                "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
            ])?;

        match response.status() {
            reqwest::StatusCode::OK => {}
            reqwest::StatusCode::NOT_MODIFIED => {
                // No new notifications
                let current_watermark =
                    watermark.unwrap_or("Thu, 01 Jan 1970 00:00:00 GMT".to_string());
                return Ok((vec![], current_watermark));
            }
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                return Err(human_errors::user(
                    "Authorization failed when trying to fetch GitHub notifications.",
                    &[
                        "Ensure that your API key is correct and has the necessary permissions to access your notifications.",
                        "If you recently changed your API key, make sure to update it in your configuration.",
                    ],
                ));
            }
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                return Err(human_errors::user(
                    "Rate limit exceeded when trying to fetch GitHub notifications.",
                    &["Wait for a while before making more requests to GitHub's API."],
                ));
            }
            status => {
                return Err(human_errors::user(
                    format!(
                        "Failed to fetch GitHub notifications. Received unexpected status code: {}",
                        status
                    ),
                    &[
                        "Make sure that your network connection is working properly.",
                        "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
                    ],
                ));
            }
        }

        let new_watermark = response
            .headers()
            .get("Last-Modified")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("Thu, 01 Jan 1970 00:00:00 GMT")
            .to_string();

        let notifications: Vec<GitHubNotificationsItem> = response.json().await.wrap_user_err(
            format!(
                "Failed to read the content of the GitHub Releases from URL '{}'.",
                &self.api_url
            ),
            &[
                "Check that the URL is correct and that the server is reachable.",
                "Check that your network connection is working properly.",
            ],
        )?;

        Ok((notifications, new_watermark))
    }
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Clone)]
pub struct GitHubNotificationsItem {
    pub id: String,
    pub reason: GitHubNotificationsReason,

    pub unread: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub last_read_at: Option<chrono::DateTime<chrono::Utc>>,

    pub repository: GitHubNotificationsRepository,
    pub subject: GitHubNotificationsSubject,
}

impl Filterable for GitHubNotificationsItem {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "reason" => serde_json::to_string(&self.reason)
                .unwrap_or_default()
                .into(),
            "repository.name" => self.repository.name.clone().into(),
            "repository.full_name" => self.repository.full_name.clone().into(),
            "repository.owner" => self.repository.owner.login.clone().into(),
            "subject.title" => self.subject.title.clone().into(),
            "subject.type" => self.subject.type_.clone().into(),

            "unread" => self.unread.into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum GitHubNotificationsReason {
    // You were requested to review and approve a deployment.
    #[serde(rename = "approval_requested")]
    ApprovalRequested,
    // You were assigned to the issue or pull request.
    #[serde(rename = "assign")]
    Assign,
    // You created the thread.
    #[serde(rename = "author")]
    Author,
    // A GitHub Actions workflow run that you triggered was completed.
    #[serde(rename = "ci_activity")]
    CiActivity,
    // You commented on the thread.
    #[serde(rename = "comment")]
    Comment,
    // You accepted an invitation to contribute to the repository.
    #[serde(rename = "invitation")]
    Invitation,
    // You subscribed to the thread.
    #[serde(rename = "manual")]
    Manual,
    // Organization members have requested to enable a feature such as Copilot.
    #[serde(rename = "member_feature_requested")]
    MemberFeatureRequested,
    // You were specifically @mentioned in the content.
    #[serde(rename = "mention")]
    Mention,
    // You, or a team you're a member of, were requested to review a pull request.
    #[serde(rename = "review_requested")]
    ReviewRequested,
    // You were credited in a security advisory.
    #[serde(rename = "security_advisory_credit")]
    SecurityAdvisoryCredit,
    // A security alert was raised for a repository you watch.
    #[serde(rename = "security_alert")]
    SecurityAlert,
    // You changed the thread state (for example, closing an issue or merging a pull request).
    #[serde(rename = "state_change")]
    StateChange,
    // You're watching the repository.
    #[serde(rename = "subscribed")]
    Subscribed,
    // You were @mentioned via a team mention.
    #[serde(rename = "team_mention")]
    TeamMention,
    // Any other reason not listed above (see https://docs.github.com/en/rest/activity/notifications?apiVersion=2022-11-28)
    #[serde(other)]
    Other,
}

impl GitHubNotificationsReason {
    pub fn priority(&self) -> i32 {
        match self {
            GitHubNotificationsReason::SecurityAlert => 4,

            GitHubNotificationsReason::ApprovalRequested => 3,
            GitHubNotificationsReason::ReviewRequested => 3,

            GitHubNotificationsReason::TeamMention => 2,
            GitHubNotificationsReason::Mention => 2,
            GitHubNotificationsReason::Subscribed => 2,
            GitHubNotificationsReason::Author => 2,

            _ => 1,
        }
    }
}

impl Display for GitHubNotificationsReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string(self)
                .unwrap_or("unknown".to_string())
                .trim_end_matches('"')
        )
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct GitHubNotificationsRepository {
    pub name: String,
    pub full_name: String,
    pub html_url: String,

    pub owner: GitHubUser,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct GitHubUser {
    pub login: String,
    pub html_url: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct GitHubNotificationsSubject {
    pub title: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub url: Option<String>,
    pub latest_comment_url: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GitHubNotificationsSubjectState {
    Open,
    Closed,
    Merged,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct GitHubSubjectInformation {
    pub state: GitHubNotificationsSubjectState,
    pub body: Option<String>,
    pub user: GitHubUser,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn test_notification_reason_serialization() {
        let examples = vec![
            (
                GitHubNotificationsReason::ApprovalRequested,
                "approval_requested",
            ),
            (GitHubNotificationsReason::Assign, "assign"),
            (GitHubNotificationsReason::Author, "author"),
            (GitHubNotificationsReason::CiActivity, "ci_activity"),
            (GitHubNotificationsReason::Comment, "comment"),
            (GitHubNotificationsReason::Invitation, "invitation"),
            (GitHubNotificationsReason::Manual, "manual"),
            (
                GitHubNotificationsReason::MemberFeatureRequested,
                "member_feature_requested",
            ),
            (GitHubNotificationsReason::Mention, "mention"),
            (
                GitHubNotificationsReason::ReviewRequested,
                "review_requested",
            ),
            (
                GitHubNotificationsReason::SecurityAdvisoryCredit,
                "security_advisory_credit",
            ),
            (GitHubNotificationsReason::SecurityAlert, "security_alert"),
            (GitHubNotificationsReason::StateChange, "state_change"),
            (GitHubNotificationsReason::Subscribed, "subscribed"),
            (GitHubNotificationsReason::TeamMention, "team_mention"),
            (GitHubNotificationsReason::Other, "Other"),
        ];

        for (reason, expected) in examples {
            let serialized = serde_json::to_string(&reason)
                .expect("Failed to serialize GitHubNotificationsReason");
            assert_eq!(serialized.trim_matches('"'), expected);

            let deserialized: GitHubNotificationsReason =
                serde_json::from_str(&format!("\"{}\"", expected))
                    .expect("Failed to deserialize GitHubNotificationsReason");
            assert_eq!(deserialized, reason);
        }
    }

    #[tokio::test]
    async fn test_list() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("github_notifications.json");

        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(test_data)
                    .insert_header("Last-Modified", "Mon, 08 Apr 2024 12:00:00 GMT"),
            )
            .mount(&mock_server)
            .await;

        let collector = GitHubNotificationsCollector::new_with_url(mock_server.uri());
        let services = crate::testing::mock_services().await.unwrap();

        let items = collector.list(&services).await.unwrap();
        assert_eq!(
            items.len(),
            3,
            "Expected to fetch 3 notification items from test data"
        );
        assert_eq!(items[0].id, "1");
        assert_eq!(items[0].subject.title, "Test Issue #1");
        assert_eq!(items[0].repository.full_name, "testorg/test-repo");
        assert_eq!(items[0].reason, GitHubNotificationsReason::Mention);
        assert!(items[0].unread);

        assert_eq!(items[1].id, "2");
        assert_eq!(items[1].subject.title, "Test PR #42");
        assert_eq!(items[1].reason, GitHubNotificationsReason::ReviewRequested);
        assert!(items[1].unread);

        assert_eq!(items[2].id, "3");
        assert_eq!(items[2].reason, GitHubNotificationsReason::Author);
        assert!(!items[2].unread);
    }

    #[tokio::test]
    async fn test_fetch_since_no_watermark() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("github_notifications.json");

        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(test_data.clone())
                    .insert_header("Last-Modified", "Mon, 08 Apr 2024 12:00:00 GMT"),
            )
            .mount(&mock_server)
            .await;

        let collector = GitHubNotificationsCollector::new_with_url(mock_server.uri());
        let services = crate::testing::mock_services().await.unwrap();

        let (items, watermark) = collector.fetch_since(None, &services).await.unwrap();

        assert_eq!(
            items.len(),
            3,
            "Expected to fetch 3 notification items from test data"
        );
        assert_eq!(watermark, "Mon, 08 Apr 2024 12:00:00 GMT");

        // Verify the If-Modified-Since header was sent correctly
        let received_requests = mock_server.received_requests().await.unwrap();
        assert_eq!(received_requests.len(), 1, "Expected exactly one request");
        let request = &received_requests[0];
        let if_modified_since = request
            .headers
            .get("if-modified-since")
            .expect("If-Modified-Since header should be present");
        assert_eq!(
            if_modified_since.to_str().unwrap(),
            "Thu, 01 Jan 1970 00:00:00 GMT",
            "If-Modified-Since header should have default watermark value"
        );
    }

    #[tokio::test]
    async fn test_fetch_since_with_watermark() {
        let mock_server = MockServer::start().await;
        let test_data = crate::testing::get_test_file_contents("github_notifications.json");

        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(test_data.clone())
                    .insert_header("Last-Modified", "Mon, 08 Apr 2024 12:00:00 GMT"),
            )
            .mount(&mock_server)
            .await;

        let collector = GitHubNotificationsCollector::new_with_url(mock_server.uri());
        let services = crate::testing::mock_services().await.unwrap();

        let watermark = Some("Mon, 01 Apr 2024 12:00:00 GMT".to_string());
        let (items, new_watermark) = collector
            .fetch_since(watermark.clone(), &services)
            .await
            .unwrap();

        assert_eq!(
            items.len(),
            3,
            "Expected to fetch 3 notification items from test data"
        );
        assert_eq!(new_watermark, "Mon, 08 Apr 2024 12:00:00 GMT");
        assert_eq!(items[0].id, "1");
        assert_eq!(items[1].id, "2");

        // Verify the If-Modified-Since header was sent correctly
        let received_requests = mock_server.received_requests().await.unwrap();
        assert_eq!(received_requests.len(), 1, "Expected exactly one request");
        let request = &received_requests[0];
        let if_modified_since = request
            .headers
            .get("if-modified-since")
            .expect("If-Modified-Since header should be present");
        assert_eq!(
            if_modified_since.to_str().unwrap(),
            "Mon, 01 Apr 2024 12:00:00 GMT",
            "If-Modified-Since header should match the provided watermark"
        );
    }

    #[tokio::test]
    async fn test_fetch_since_not_modified() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(304))
            .mount(&mock_server)
            .await;

        let collector = GitHubNotificationsCollector::new_with_url(mock_server.uri());
        let services = crate::testing::mock_services().await.unwrap();

        let watermark = Some("Mon, 08 Apr 2024 12:00:00 GMT".to_string());
        let (items, new_watermark) = collector
            .fetch_since(watermark.clone(), &services)
            .await
            .unwrap();

        assert_eq!(items.len(), 0, "Expected no items when not modified");
        assert_eq!(
            new_watermark, "Mon, 08 Apr 2024 12:00:00 GMT",
            "Watermark should remain unchanged"
        );

        // Verify the If-Modified-Since header was sent correctly
        let received_requests = mock_server.received_requests().await.unwrap();
        assert_eq!(received_requests.len(), 1, "Expected exactly one request");
        let request = &received_requests[0];
        let if_modified_since = request
            .headers
            .get("if-modified-since")
            .expect("If-Modified-Since header should be present");
        assert_eq!(
            if_modified_since.to_str().unwrap(),
            "Mon, 08 Apr 2024 12:00:00 GMT",
            "If-Modified-Since header should match the provided watermark"
        );
    }
}
