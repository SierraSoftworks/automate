use crate::config::TodoistConfig;
use crate::prelude::*;
use crate::publishers::{TodoistUpsertTask, TodoistUpsertTaskPayload};
use crate::services::{AutoMergeOutcome, GitHubClient};
use crate::webhooks::GitHubPullRequestEvent;

#[derive(Clone, Deserialize)]
pub struct GitHubAutoMergeConfig {
    /// Selects the pull requests which should have auto-merge enabled. Defaults
    /// to newly opened Dependabot pull requests.
    #[serde(default = "default_auto_merge_filter")]
    pub filter: Filter,

    /// Whether to leave an approving review on matching pull requests, which
    /// repositories with a required-review branch protection rule need before
    /// auto-merge can complete.
    #[serde(default)]
    pub approve: bool,

    #[serde(default = "default_approval_message")]
    pub approval_message: String,

    /// Where the reminders to turn on a repository's "Allow auto-merge" setting
    /// are filed.
    #[serde(default)]
    pub todoist: TodoistConfig,
}

impl Default for GitHubAutoMergeConfig {
    fn default() -> Self {
        Self {
            filter: default_auto_merge_filter(),
            approve: false,
            approval_message: default_approval_message(),
            todoist: TodoistConfig::default(),
        }
    }
}

fn default_auto_merge_filter() -> Filter {
    Filter::new(r#"action == "opened" && author in ["dependabot[bot]", "dependabot-preview[bot]"]"#)
        .expect("the default auto-merge filter is always valid")
}

fn default_approval_message() -> String {
    "This pull request has been automatically approved because it was raised by a trusted account."
        .to_string()
}

/// Enables GitHub's native auto-merge behaviour on pull requests raised by
/// trusted accounts, so that they merge themselves once their required checks
/// have passed.
#[derive(Clone)]
pub struct GitHubAutoMergeWorkflow;

impl GitHubAutoMergeWorkflow {
    /// Raises a reminder to turn on the repository's "Allow auto-merge"
    /// setting.
    ///
    /// The payload is keyed on, and derived solely from, the repository, so
    /// every subsequent pull request from the same repository resolves to an
    /// unchanged upsert which Todoist is never told about.
    async fn request_repository_configuration(
        event: &GitHubPullRequestEvent,
        config: &GitHubAutoMergeConfig,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<(), human_errors::Error> {
        let repository = &event.repository.full_name;
        let unique_key = format!("github/auto-merge/{repository}");

        TodoistUpsertTask::dispatch(
            TodoistUpsertTaskPayload {
                unique_key: unique_key.clone(),
                title: format!(
                    "[**{repository}**](https://github.com/{repository}/settings): Enable auto-merge"
                ),
                description: Some(format!(
                    "Auto-merge could not be enabled on a pull request because {repository} does not allow it.\n\nTurn on **Allow auto-merge** under https://github.com/{repository}/settings."
                )),
                priority: Some(2),
                config: config.todoist.clone(),
                ..Default::default()
            },
            Some(unique_key.into()),
            services,
        )
        .await
    }
}

crate::register_job!(GitHubAutoMergeWorkflow);

impl Job for GitHubAutoMergeWorkflow {
    type JobType = GitHubPullRequestEvent;

    fn partition() -> &'static str {
        "github/auto-merge"
    }

    #[instrument("workflow.github_auto_merge.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let config = services.config();

        // The configuration may have changed between the webhook being received
        // and this job running, so re-check rather than assume.
        let Some(auto_merge) = config.webhooks.github.auto_merge.as_ref() else {
            debug!("Auto-merge is not configured; ignoring {job}.");
            return Ok(());
        };

        if !auto_merge.filter.matches(job)? {
            info!("Pull request {job} did not match the auto-merge filter; ignoring.");
            return Ok(());
        }

        let Some(api_key) = config.connections.github.api_key.as_ref() else {
            warn!(
                "Pull request {job} matched the auto-merge filter, but no connections.github.api_key is configured; ignoring."
            );
            return Ok(());
        };

        let client = GitHubClient::new(services.http_client(), api_key);

        if auto_merge.approve
            && !client
                .approve_pull_request(&job.pull_request.node_id, &auto_merge.approval_message)
                .await?
        {
            warn!("Could not approve pull request {job}; continuing to enable auto-merge anyway.");
        }

        match client.enable_auto_merge(&job.pull_request.node_id).await? {
            AutoMergeOutcome::Enabled => {
                info!("Enabled auto-merge on pull request {job}.");
            }
            AutoMergeOutcome::NotAllowed => {
                warn!(
                    "Auto-merge is not allowed on {}; raising a reminder to enable it.",
                    job.repository.full_name
                );
                Self::request_repository_configuration(job, auto_merge, services).await?;
            }
            AutoMergeOutcome::Declined(reason) => {
                warn!("Auto-merge could not be enabled on pull request {job}: {reason}");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhooks::GitHubWebhookConfig;

    fn event(action: &str, author: &str) -> GitHubPullRequestEvent {
        serde_json::from_value(serde_json::json!({
            "action": action,
            "number": 1,
            "pull_request": {
                "node_id": "PR_node",
                "html_url": "https://github.com/example/repo/pull/1",
                "title": "Bump serde",
                "draft": false,
                "user": { "login": author },
            },
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "private": false,
                "owner": { "login": "example" },
            },
            "sender": { "login": author },
        }))
        .expect("the sample pull_request event should deserialize")
    }

    fn matches(action: &str, author: &str) -> bool {
        default_auto_merge_filter()
            .matches(&event(action, author))
            .expect("the default filter should evaluate")
    }

    #[test]
    fn default_filter_selects_new_dependabot_pull_requests() {
        assert!(matches("opened", "dependabot[bot]"));
        assert!(matches("opened", "dependabot-preview[bot]"));
        assert!(!matches("synchronize", "dependabot[bot]"));
        assert!(!matches("opened", "notheotherben"));
    }

    #[tokio::test]
    async fn repositories_without_auto_merge_raise_a_single_reminder() {
        let services = crate::testing::mock_services()
            .await
            .expect("build mock services");
        let config = GitHubAutoMergeConfig::default();

        let first = event("opened", "dependabot[bot]");
        let mut second = event("opened", "dependabot[bot]");
        second.number = 2;
        second.pull_request.node_id = "PR_node_2".to_string();

        for pull_request in [&first, &second] {
            GitHubAutoMergeWorkflow::request_repository_configuration(
                pull_request,
                &config,
                &services,
            )
            .await
            .expect("the reminder should be raised");
        }

        let reminders = services
            .queue()
            .peek::<_, TodoistUpsertTaskPayload>(TodoistUpsertTask::partition(), 10)
            .await
            .expect("peek the Todoist upsert queue");

        assert_eq!(
            reminders.len(),
            1,
            "every pull request from a repository should collapse onto one reminder"
        );
        assert_eq!(reminders[0].key, "github/auto-merge/example/repo");
        assert_eq!(
            reminders[0].payload.title,
            "[**example/repo**](https://github.com/example/repo/settings): Enable auto-merge"
        );
    }

    #[tokio::test]
    async fn unmatched_pull_requests_are_ignored() {
        // No GitHub API key is configured, so a call to GitHub would fail; the
        // job completing successfully proves the filter short-circuited first.
        let services = crate::services::ServicesContainer::new_custom_mock(|config, _| {
            config.webhooks.github = GitHubWebhookConfig {
                secret: "secret".to_string(),
                auto_merge: Some(GitHubAutoMergeConfig::default()),
            };
        })
        .await
        .expect("build mock services");

        GitHubAutoMergeWorkflow
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &event("opened", "notheotherben"),
            )
            .await
            .expect("an unmatched pull request should be ignored without erroring");
    }
}
