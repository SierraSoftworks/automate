use crate::prelude::*;
use crate::publishers::TodoistTarget;
use crate::publishers::{TodoistUpsertTask, TodoistUpsertTaskPayload};
use crate::services::{AutoMergeOutcome, GitHubAppClient, GitHubClient};
use crate::webhooks::GitHubPullRequestEvent;

/// What the GitHub webhook hands this job.
///
/// The settings travel with the event rather than being read from the
/// installation's configuration, because they belong to the workflow the
/// delivery arrived for.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct GitHubAutoMergeTask {
    pub config: GitHubAutoMergeConfig,
    pub event: GitHubPullRequestEvent,
}

impl std::fmt::Display for GitHubAutoMergeTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "github/auto-merge")
    }
}

/// The pull requests an untouched auto-merge section acts on.
///
/// Held as its source text rather than only as a built [`Filter`], so that the
/// form's default and the `serde(default)` are literally the same string. The
/// two used to be unable to disagree because the form did not offer the field
/// at all; now that it does, sharing the constant is what keeps "left the form
/// alone" and "left the section out" meaning the same thing.
pub const DEFAULT_AUTO_MERGE_FILTER: &str =
    r#"action == "opened" && author in ["dependabot[bot]", "dependabot-preview[bot]"]"#;

/// The review body left on a pull request when approval is switched on.
pub const DEFAULT_APPROVAL_MESSAGE: &str =
    "This pull request has been automatically approved because it was raised by a trusted account.";

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubAutoMergeConfig {
    /// Whether this workflow enables auto-merge at all.
    ///
    /// Off unless somebody says so. This is the setting that has us approve and
    /// merge pull requests in their repositories, which is not a capability
    /// anybody should acquire by upgrading — and an explicit switch is also what
    /// lets the rest of these settings be reached by a dotted path, which an
    /// `Option` could not be.
    #[serde(default)]
    pub enabled: bool,

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
    pub todoist: TodoistTarget,
}

impl Default for GitHubAutoMergeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            filter: default_auto_merge_filter(),
            approve: false,
            approval_message: default_approval_message(),
            todoist: TodoistTarget::default(),
        }
    }
}

fn default_auto_merge_filter() -> Filter {
    Filter::new(DEFAULT_AUTO_MERGE_FILTER).expect("the default auto-merge filter is always valid")
}

fn default_approval_message() -> String {
    DEFAULT_APPROVAL_MESSAGE.to_string()
}

/// Enables GitHub's native auto-merge behaviour on pull requests raised by
/// trusted accounts, so that they merge themselves once their required checks
/// have passed.
#[derive(Clone)]
pub struct GitHubAutoMergeWorkflow;

impl GitHubAutoMergeWorkflow {
    /// The credential management calls are made with.
    ///
    /// Prefers an App installation token, so the approval and merge are
    /// attributed to the App and limited to the repositories that installation
    /// covers. Falls back to the personal access token when the App is not
    /// configured, or when the delivery came from a plain repository webhook
    /// and so carries no installation.
    async fn management_token(
        event: &GitHubPullRequestEvent,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<Option<String>, human_errors::Error> {
        let config = services.config();

        if let Some(app) = config.connections.github.app.as_ref()
            && let Some(installation) = event.installation.as_ref()
        {
            let client = GitHubAppClient::new(app, services.http_client())?;
            return Ok(Some(
                client.installation_token(installation.id, services).await?,
            ));
        }

        Ok(config.connections.github.api_key.clone())
    }

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
    type JobType = GitHubAutoMergeTask;

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
        let auto_merge = &job.config;
        let job = &job.event;

        if !auto_merge.filter.matches(job)? {
            info!("Pull request {job} did not match the auto-merge filter; ignoring.");
            return Ok(());
        }

        let Some(token) = Self::management_token(job, services).await? else {
            warn!(
                "Pull request {job} matched the auto-merge filter, but no GitHub credentials are configured; ignoring."
            );
            return Ok(());
        };

        let client = GitHubClient::new(services.http_client(), token);

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

    #[test]
    fn a_section_nobody_has_filled_in_is_switched_off() {
        // The switch replaced an `Option` whose presence turned this on, so the
        // reading of an empty object had to change. Off is the only safe answer:
        // this approves and merges pull requests in somebody's repositories.
        let empty: GitHubAutoMergeConfig = serde_json::from_value(serde_json::json!({}))
            .expect("an empty section should load at its defaults");

        assert!(!empty.enabled);
        assert!(!GitHubAutoMergeConfig::default().enabled);
    }

    #[test]
    fn the_settings_survive_being_stored_and_read_back() {
        // These are part of a workflow's stored configuration now rather than an
        // internal job's private state, so they have to write as well as read —
        // and a filter that did not round-trip would be silently replaced with
        // its default the next time the workflow was saved.
        let config = GitHubAutoMergeConfig {
            enabled: true,
            approve: true,
            filter: Filter::new(r#"author == "notheotherben""#).unwrap(),
            ..Default::default()
        };

        let round_tripped: GitHubAutoMergeConfig = serde_json::from_value(
            serde_json::to_value(&config).expect("the settings should write"),
        )
        .expect("the settings should read back");

        assert!(round_tripped.enabled);
        assert!(round_tripped.approve);
        assert_eq!(round_tripped.filter.raw(), r#"author == "notheotherben""#);
        assert_eq!(round_tripped.approval_message, DEFAULT_APPROVAL_MESSAGE);
    }

    #[test]
    fn the_advertised_defaults_are_the_ones_the_struct_uses() {
        // The form offers these constants as its starting values, so if they
        // stopped being what an omitted section falls back to then an untouched
        // form and a missing section would behave differently.
        let empty: GitHubAutoMergeConfig = serde_json::from_value(serde_json::json!({})).unwrap();

        assert_eq!(empty.filter.raw(), DEFAULT_AUTO_MERGE_FILTER);
        assert_eq!(empty.approval_message, DEFAULT_APPROVAL_MESSAGE);
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
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .expect("build mock services");

        GitHubAutoMergeWorkflow
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &GitHubAutoMergeTask {
                    config: GitHubAutoMergeConfig::default(),
                    event: event("opened", "notheotherben"),
                },
            )
            .await
            .expect("an unmatched pull request should be ignored without erroring");
    }
}
