use human_errors::ResultExt;
use serde::{Deserialize, Serialize};

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

    pub async fn get_subject_state(&self, subject: &GitHubNotificationsSubject, services: &(impl crate::services::Services + Send + Sync + 'static)) -> Result<GitHubNotificationsSubjectState, human_errors::Error> {
        if let Some(url) = &subject.url {
            let client = self.get_client(services)?;

            let response = client.get(url)
                .send().await.wrap_err_as_user("We were unable to fetch GitHub notification subject state from GitHub.", &[
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
                        &[
                            "Wait for a while before making more requests to GitHub's API.",
                        ],
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

            let issue: GitHubSubjectStatusItem = response.json().await.wrap_err_as_user(
                format!(
                    "Failed to read the content of the GitHub notification subject from URL '{}'.",
                    url
                ),
                &[
                    "Check that the URL is correct and that the server is reachable.",
                    "Check that your network connection is working properly.",
                ],
            )?;

            Ok(issue.state)
        } else {
            Ok(GitHubNotificationsSubjectState::Open)
        }
    }

    pub async fn mark_as_done(&self, thread_id: &str, services: &(impl crate::services::Services + Send + Sync + 'static)) -> Result<(), human_errors::Error> {
        let client = self.get_client(services)?;

        let response = client
            .delete(format!("{}/notifications/threads/{}", self.api_url, thread_id))
            .send().await.wrap_err_as_user("We were unable to mark the GitHub notification as read.", &[
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
            status => {
                Err(human_errors::user(
                    format!(
                        "Failed to mark GitHub notification as read. Received unexpected status code: {}",
                        status
                    ),
                    &[
                        "Make sure that your network connection is working properly.",
                        "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
                    ],
                ))
            }
        }
    }

    fn get_client(&self, services: &impl crate::services::Services) -> Result<reqwest::Client, human_errors::Error> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("X-GitHub-Api-Version", "2022-11-28".parse().unwrap());
        headers.insert("Accept", "application/vnd.github+json".parse().unwrap());

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

        Ok(client)
    }
}

#[async_trait::async_trait]
impl Collector for GitHubNotificationsCollector {
    type Item = GitHubNotificationsItem;

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

    async fn fetch_since(
        &self,
        watermark: Option<Self::Watermark>,
        services: &impl crate::services::Services,
    ) -> Result<(Vec<Self::Item>, Self::Watermark), human_errors::Error> {
        let client = self.get_client(services)?;

        let response = client.get(format!("{}/notifications", self.api_url))
            .header("If-Modified-Since", watermark.as_deref().unwrap_or("Thu, 01 Jan 1970 00:00:00 GMT"))
            .send().await.wrap_err_as_user("We were unable to fetch GitHub notifications from GitHub.", &[
                "Make sure that your network connection is working properly.",
                "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
            ])?;

        match response.status() {
            reqwest::StatusCode::OK => {}
            reqwest::StatusCode::NOT_MODIFIED => {
                // No new notifications
                let current_watermark = watermark.unwrap_or("Thu, 01 Jan 1970 00:00:00 GMT".to_string());
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
                    &[
                        "Wait for a while before making more requests to GitHub's API.",
                    ],
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

        let new_watermark = response.headers().get("Last-Modified")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("Thu, 01 Jan 1970 00:00:00 GMT")
            .to_string();

        let notifications: Vec<GitHubNotificationsItem> = response.json().await.wrap_err_as_user(
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

    pub repository : GitHubNotificationsRepository,
    pub subject: GitHubNotificationsSubject,
}

impl Filterable for GitHubNotificationsItem {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "reason" => serde_json::to_string(&self.reason).unwrap_or_default().into(),
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
    Other
}

impl GitHubNotificationsReason {
    pub fn priority(&self) -> i32 {
        match self {
            GitHubNotificationsReason::SecurityAlert => 4,

            GitHubNotificationsReason::ApprovalRequested => 3,
            GitHubNotificationsReason::Assign => 3,
            GitHubNotificationsReason::Mention => 3,
            GitHubNotificationsReason::TeamMention => 3,
            GitHubNotificationsReason::ReviewRequested => 3,
            
            GitHubNotificationsReason::Subscribed => 2,
            GitHubNotificationsReason::Comment => 2,
            GitHubNotificationsReason::Author => 2,

            _ => 1
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct GitHubNotificationsRepository {
    pub name: String,
    pub full_name: String,
    pub html_url: String,

    pub owner: GitHubNotificationsRepositoryOwner,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct GitHubNotificationsRepositoryOwner {
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

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum GitHubNotificationsSubjectState {
    Open,
    Closed,
    Merged,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct GitHubSubjectStatusItem {
    pub id: u64,
    pub state: GitHubNotificationsSubjectState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_reason_serialization() {
        let examples = vec![
            (GitHubNotificationsReason::ApprovalRequested, "approval_requested"),
            (GitHubNotificationsReason::Assign, "assign"),
            (GitHubNotificationsReason::Author, "author"),
            (GitHubNotificationsReason::CiActivity, "ci_activity"),
            (GitHubNotificationsReason::Comment, "comment"),
            (GitHubNotificationsReason::Invitation, "invitation"),
            (GitHubNotificationsReason::Manual, "manual"),
            (GitHubNotificationsReason::MemberFeatureRequested, "member_feature_requested"),
            (GitHubNotificationsReason::Mention, "mention"),
            (GitHubNotificationsReason::ReviewRequested, "review_requested"),
            (GitHubNotificationsReason::SecurityAdvisoryCredit, "security_advisory_credit"),
            (GitHubNotificationsReason::SecurityAlert, "security_alert"),
            (GitHubNotificationsReason::StateChange, "state_change"),
            (GitHubNotificationsReason::Subscribed, "subscribed"),
            (GitHubNotificationsReason::TeamMention, "team_mention"),
            (GitHubNotificationsReason::Other, "Other"),
        ];

        for (reason, expected) in examples {
            let serialized =
                serde_json::to_string(&reason).expect("Failed to serialize GitHubNotificationsReason");
            assert_eq!(serialized.trim_matches('"'), expected);

            let deserialized: GitHubNotificationsReason =
                serde_json::from_str(&format!("\"{}\"", expected))
                    .expect("Failed to deserialize GitHubNotificationsReason");
            assert_eq!(deserialized, reason);
        }
    }
}