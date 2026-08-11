use crate::prelude::*;

const GITHUB_GRAPHQL_URL: &str = "https://api.github.com/graphql";

/// A minimal GitHub GraphQL API client, used for the operations which have no
/// REST equivalent (enabling a pull request's auto-merge behaviour and
/// submitting reviews).
#[derive(Clone)]
pub struct GitHubClient {
    http_client: reqwest::Client,
    api_key: String,
    graphql_url: String,
}

impl GitHubClient {
    pub fn new(http_client: reqwest::Client, api_key: impl Into<String>) -> Self {
        Self {
            http_client,
            api_key: api_key.into(),
            graphql_url: GITHUB_GRAPHQL_URL.to_string(),
        }
    }

    #[cfg(test)]
    pub fn new_with_url(
        http_client: reqwest::Client,
        api_key: impl Into<String>,
        graphql_url: impl Into<String>,
    ) -> Self {
        Self {
            http_client,
            api_key: api_key.into(),
            graphql_url: graphql_url.into(),
        }
    }

    /// Enables GitHub's native auto-merge behaviour on a pull request, merging
    /// it once its required checks and reviews have passed.
    ///
    /// A rejection by GitHub is a permanent condition for the delivery in
    /// question, so it is reported as an [`AutoMergeOutcome`] rather than
    /// raised as a retryable error.
    #[instrument("services.github.enable_auto_merge", skip(self), err(Display))]
    pub async fn enable_auto_merge(
        &self,
        pull_request_id: &str,
    ) -> Result<AutoMergeOutcome, human_errors::Error> {
        let response: GraphQlResponse<EnableAutoMergeData> = self
            .graphql(
                "enablePullRequestAutoMerge",
                r#"mutation EnableAutoMerge($pullRequest: ID!) {
                    enablePullRequestAutoMerge(input: { pullRequestId: $pullRequest }) {
                        pullRequest {
                            autoMergeRequest {
                                enabledAt
                            }
                        }
                    }
                }"#,
                serde_json::json!({ "pullRequest": pull_request_id }),
            )
            .await?;

        if !response.errors.is_empty() {
            let reason = response.error_summary();
            let reason_lower = reason.to_lowercase();

            // GitHub reports a repository without the "Allow auto-merge"
            // setting as "Pull request Auto merge is not allowed for this
            // repository".
            if reason_lower.contains("auto merge is not allowed") {
                return Ok(AutoMergeOutcome::NotAllowed);
            }

            // "Pull request Pull request is in clean status" means there is
            // nothing left for a scheduled merge to wait on, and GitHub will not
            // schedule one that would fire immediately. Common on a repository
            // with no required status checks, which is why auto-merge looked
            // like it was never running.
            if reason_lower.contains("clean status") {
                return Ok(AutoMergeOutcome::Mergeable);
            }

            if is_forbidden(&reason_lower) {
                return Ok(AutoMergeOutcome::Forbidden);
            }

            return Ok(AutoMergeOutcome::Declined(reason));
        }

        if response
            .data
            .and_then(|d| d.enable_pull_request_auto_merge)
            .and_then(|m| m.pull_request)
            .and_then(|pr| pr.auto_merge_request)
            .is_some_and(|amr| amr.enabled_at.is_some())
        {
            Ok(AutoMergeOutcome::Enabled)
        } else {
            Ok(AutoMergeOutcome::Declined(
                "GitHub did not report an auto-merge request on the pull request.".to_string(),
            ))
        }
    }

    /// Merges a pull request outright, returning `false` when GitHub declines.
    ///
    /// Only used when [`AutoMergeOutcome::Mergeable`] says a scheduled merge was
    /// refused for having nothing to wait on. Branch protection is still applied
    /// by GitHub, so this cannot merge anything auto-merge would have held back.
    #[instrument("services.github.merge_pull_request", skip(self), err(Display))]
    pub async fn merge_pull_request(
        &self,
        pull_request_id: &str,
    ) -> Result<bool, human_errors::Error> {
        let response: GraphQlResponse<MergePullRequestData> = self
            .graphql(
                "mergePullRequest",
                r#"mutation MergePullRequest($pullRequest: ID!) {
                    mergePullRequest(input: { pullRequestId: $pullRequest }) {
                        pullRequest {
                            merged
                        }
                    }
                }"#,
                serde_json::json!({ "pullRequest": pull_request_id }),
            )
            .await?;

        if !response.errors.is_empty() {
            let reason = response.error_summary();
            warn!(
                "GitHub declined to merge pull request '{pull_request_id}': {reason}{}",
                forbidden_advice(&reason)
            );
            return Ok(false);
        }

        Ok(response
            .data
            .and_then(|d| d.merge_pull_request)
            .and_then(|m| m.pull_request)
            .is_some_and(|pr| pr.merged))
    }

    /// Submits an approving review on a pull request, returning `false` when
    /// GitHub declines the review (for example because the token's user has
    /// already reviewed it, or cannot approve their own pull request).
    #[instrument(
        "services.github.approve_pull_request",
        skip(self, comment),
        err(Display)
    )]
    pub async fn approve_pull_request(
        &self,
        pull_request_id: &str,
        comment: &str,
    ) -> Result<bool, human_errors::Error> {
        let response: GraphQlResponse<AddPullRequestReviewData> = self
            .graphql(
                "addPullRequestReview",
                r#"mutation ApprovePullRequest($pullRequest: ID!, $comment: String!) {
                    addPullRequestReview(input: {
                        pullRequestId: $pullRequest,
                        body: $comment,
                        event: APPROVE
                    }) {
                        pullRequestReview {
                            id
                        }
                    }
                }"#,
                serde_json::json!({ "pullRequest": pull_request_id, "comment": comment }),
            )
            .await?;

        if !response.errors.is_empty() {
            let reason = response.error_summary();
            warn!(
                "GitHub declined to approve pull request '{pull_request_id}': {reason}{}",
                forbidden_advice(&reason)
            );
            return Ok(false);
        }

        Ok(response
            .data
            .and_then(|d| d.add_pull_request_review)
            .and_then(|r| r.pull_request_review)
            .is_some())
    }

    #[instrument("services.github.graphql", skip(self, query, variables), fields(otel.kind=?OpenTelemetrySpanKind::Client, rpc.system = "graphql", rpc.service = "github", rpc.method = operation))]
    async fn graphql<T: DeserializeOwned>(
        &self,
        operation: &str,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<GraphQlResponse<T>, human_errors::Error> {
        let response = self
            .http_client
            .post(&self.graphql_url)
            .bearer_auth(&self.api_key)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&serde_json::json!({ "query": query, "variables": variables }))
            .send()
            .await
            .wrap_user_err(
                format!("We were unable to run the GitHub GraphQL operation '{operation}'."),
                &[
                    "Make sure that your network connection is working properly.",
                    "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
                ],
            )?;

        match response.status() {
            reqwest::StatusCode::OK => {}
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                return Err(human_errors::user(
                    format!(
                        "Authorization failed when running the GitHub GraphQL operation '{operation}'."
                    ),
                    &[
                        "Ensure that the selected GitHub connection contains a valid personal access token.",
                        "Make sure that the token has the `repo` scope for the repositories you want to manage.",
                    ],
                ));
            }
            status => {
                return Err(human_errors::user(
                    format!(
                        "The GitHub GraphQL operation '{operation}' failed with an unexpected status code: {status}"
                    ),
                    &[
                        "Make sure that your network connection is working properly.",
                        "Check https://www.githubstatus.com/ for any ongoing issues with GitHub's services.",
                    ],
                ));
            }
        }

        response.json().await.wrap_system_err(
            format!("Failed to read the response to the GitHub GraphQL operation '{operation}'."),
            &[
                "This usually means GitHub's response format differs from the model we expect.",
                "Please report this issue to the dev team on GitHub so the model can be updated.",
            ],
        )
    }
}

/// Whether GitHub refused because the App installation may not touch the thing
/// being addressed.
///
/// Spelled "Resource not accessible by integration", and it covers three
/// situations a person has to fix by hand: the permission was never granted,
/// it was added to the App after this account installed it and nobody has
/// accepted the change, or the repository is not one the installation covers.
fn is_forbidden(reason: &str) -> bool {
    reason
        .to_lowercase()
        .contains("not accessible by integration")
}

/// What to append to a refusal which turns out to be about permissions, since
/// GitHub's own wording says nothing about what to do next.
fn forbidden_advice(reason: &str) -> &'static str {
    if is_forbidden(reason) {
        " Give the App installation Read & write access to Pull requests and Contents, and check that it covers this repository."
    } else {
        ""
    }
}

/// The result of asking GitHub to enable auto-merge on a pull request.
#[derive(Debug, PartialEq, Eq)]
pub enum AutoMergeOutcome {
    Enabled,

    /// The pull request has nothing left to wait for, so GitHub refused to
    /// schedule a merge for it. Auto-merge would have merged it on the spot, so
    /// merging it directly is the same outcome by the only route left.
    Mergeable,

    /// The App installation may not act on this repository, which no amount of
    /// retrying will change.
    Forbidden,

    /// The repository does not have its "Allow auto-merge" setting turned on,
    /// which is a repository-level problem rather than a per-pull-request one.
    NotAllowed,

    /// GitHub declined for some other reason, reported verbatim.
    Declined(String),
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "T: DeserializeOwned"))]
struct GraphQlResponse<T> {
    #[serde(default)]
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

impl<T> GraphQlResponse<T> {
    fn error_summary(&self) -> String {
        self.errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[derive(Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Deserialize)]
struct EnableAutoMergeData {
    #[serde(rename = "enablePullRequestAutoMerge")]
    enable_pull_request_auto_merge: Option<EnableAutoMergePayload>,
}

#[derive(Deserialize)]
struct EnableAutoMergePayload {
    #[serde(rename = "pullRequest")]
    pull_request: Option<AutoMergePullRequest>,
}

#[derive(Deserialize)]
struct AutoMergePullRequest {
    #[serde(rename = "autoMergeRequest")]
    auto_merge_request: Option<AutoMergeRequest>,
}

#[derive(Deserialize)]
struct AutoMergeRequest {
    #[serde(rename = "enabledAt")]
    enabled_at: Option<String>,
}

#[derive(Deserialize)]
struct MergePullRequestData {
    #[serde(rename = "mergePullRequest")]
    merge_pull_request: Option<MergePullRequestPayload>,
}

#[derive(Deserialize)]
struct MergePullRequestPayload {
    #[serde(rename = "pullRequest")]
    pull_request: Option<MergedPullRequest>,
}

#[derive(Deserialize)]
struct MergedPullRequest {
    #[serde(default)]
    merged: bool,
}

#[derive(Deserialize)]
struct AddPullRequestReviewData {
    #[serde(rename = "addPullRequestReview")]
    add_pull_request_review: Option<AddPullRequestReviewPayload>,
}

#[derive(Deserialize)]
struct AddPullRequestReviewPayload {
    #[serde(rename = "pullRequestReview")]
    pull_request_review: Option<PullRequestReview>,
}

#[derive(Deserialize)]
struct PullRequestReview {
    #[allow(dead_code)]
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mock_client(server: &MockServer) -> GitHubClient {
        let services = crate::testing::mock_services()
            .await
            .expect("build mock services");

        GitHubClient::new_with_url(
            services.http_client(),
            "test-token",
            format!("{}/graphql", server.uri()),
        )
    }

    #[tokio::test]
    async fn enable_auto_merge_success() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "enablePullRequestAutoMerge": {
                        "pullRequest": {
                            "autoMergeRequest": {
                                "enabledAt": "2024-01-01T00:00:00Z"
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert_eq!(
            client
                .enable_auto_merge("PR_node_id")
                .await
                .expect("the mutation should succeed"),
            AutoMergeOutcome::Enabled
        );
    }

    /// A repository with no required status checks leaves every pull request in
    /// "clean status", which GitHub refuses to schedule a merge for. Reading
    /// that as a plain refusal is what made auto-merge look like it never ran.
    #[tokio::test]
    async fn enable_auto_merge_detects_a_pull_request_with_nothing_to_wait_for() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": null,
                "errors": [{ "message": "Pull request Pull request is in clean status" }]
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert_eq!(
            client
                .enable_auto_merge("PR_node_id")
                .await
                .expect("a GraphQL error should not be raised as a retryable failure"),
            AutoMergeOutcome::Mergeable
        );
    }

    /// GitHub says only "Resource not accessible by integration", which is the
    /// same wording whether a permission is missing or the repository is not
    /// one the installation covers. Either way retrying will never fix it, so
    /// it has to be told apart from a refusal about the pull request itself.
    #[tokio::test]
    async fn enable_auto_merge_detects_an_installation_without_the_access_it_needs() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": null,
                "errors": [{ "message": "Resource not accessible by integration" }]
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert_eq!(
            client
                .enable_auto_merge("PR_node_id")
                .await
                .expect("a GraphQL error should not be raised as a retryable failure"),
            AutoMergeOutcome::Forbidden
        );
    }

    #[tokio::test]
    async fn merge_pull_request_reports_whether_it_merged() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "mergePullRequest": { "pullRequest": { "merged": true } } }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(
            client
                .merge_pull_request("PR_node_id")
                .await
                .expect("the mutation should succeed")
        );
    }

    #[tokio::test]
    async fn merge_pull_request_reports_a_refusal_without_raising() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": null,
                "errors": [{ "message": "Pull request is not mergeable" }]
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(
            !client
                .merge_pull_request("PR_node_id")
                .await
                .expect("a GraphQL error should not be raised as a retryable failure")
        );
    }

    #[tokio::test]
    async fn enable_auto_merge_detects_repositories_without_auto_merge() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": null,
                "errors": [{ "message": "Pull request Auto merge is not allowed for this repository" }]
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert_eq!(
            client
                .enable_auto_merge("PR_node_id")
                .await
                .expect("a GraphQL error should not be raised as a retryable failure"),
            AutoMergeOutcome::NotAllowed
        );
    }

    #[tokio::test]
    async fn enable_auto_merge_reports_graphql_errors() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": null,
                "errors": [{ "message": "Could not resolve to a node with the global id of 'PR_node_id'" }]
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(matches!(
            client
                .enable_auto_merge("PR_node_id")
                .await
                .expect("a GraphQL error should not be raised as a retryable failure"),
            AutoMergeOutcome::Declined(_)
        ));
    }

    #[tokio::test]
    async fn enable_auto_merge_raises_authorization_failures() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(client.enable_auto_merge("PR_node_id").await.is_err());
    }

    #[tokio::test]
    async fn approve_pull_request_success() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "addPullRequestReview": {
                        "pullRequestReview": { "id": "PRR_node_id" }
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(
            client
                .approve_pull_request("PR_node_id", "Approved")
                .await
                .expect("the mutation should succeed")
        );
    }
}
