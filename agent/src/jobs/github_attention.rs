use crate::config::TodoistConfig;
use crate::prelude::*;
use crate::publishers::{
    TodoistCompleteTask, TodoistCompleteTaskPayload, TodoistDueDate, TodoistUpsertTask,
    TodoistUpsertTaskPayload,
};
use crate::webhooks::{GitHubAttentionEvent, GitHubAttentionKind};

/// How much of a comment body is carried into the Todoist task before it stops
/// being a summary.
const MAX_BODY: usize = 500;

/// The Todoist key tracking a GitHub issue or pull request.
///
/// Both the webhook-driven attention path and the notification collector resolve
/// to this, so one issue yields one task no matter which noticed it first. It is
/// deliberately derived from the subject rather than from a notification thread
/// id, because a thread identifies the *notification* and one issue accumulates
/// several threads over its life.
pub fn subject_key(repository: &str, number: u64) -> String {
    format!("github/attention/{repository}#{number}")
}

#[derive(Clone, Deserialize)]
pub struct GitHubAttentionConfig {
    /// Selects the comments and reviews worth raising a task for. Defaults to
    /// everything except Dependabot's own commentary; add yourself so that your
    /// own replies do not nag you.
    #[serde(default = "default_comment_filter")]
    pub comments: Filter,

    /// Selects the assignments worth raising a task for. Defaults to nothing,
    /// because only you know which account is yours.
    #[serde(default = "default_assignment_filter")]
    pub assignments: Filter,

    /// Selects the security alerts worth raising a task for. Defaults to all of
    /// them.
    #[serde(default)]
    pub security_alerts: Filter,

    #[serde(default)]
    pub todoist: TodoistConfig,
}

impl Default for GitHubAttentionConfig {
    fn default() -> Self {
        Self {
            comments: default_comment_filter(),
            assignments: default_assignment_filter(),
            security_alerts: Filter::default(),
            todoist: TodoistConfig::default(),
        }
    }
}

fn default_comment_filter() -> Filter {
    Filter::new(r#"!(author in ["dependabot[bot]", "dependabot-preview[bot]"])"#)
        .expect("the default comment filter is always valid")
}

fn default_assignment_filter() -> Filter {
    Filter::new("false").expect("the literal `false` filter is always valid")
}

/// Raises a Todoist task when an issue, pull request or repository needs the
/// user's attention.
#[derive(Clone)]
pub struct GitHubAttentionWorkflow;

impl GitHubAttentionWorkflow {
    fn title(event: &GitHubAttentionEvent) -> String {
        match event.kind {
            GitHubAttentionKind::SecurityAlert => {
                format!("[**{}**]({}): {}", event.repository, event.url, event.title)
            }
            _ => format!(
                "[**{}#{}**]({}): {}",
                event.repository, event.number, event.url, event.title
            ),
        }
    }

    fn description(event: &GitHubAttentionEvent) -> String {
        let actor = event.actor.as_deref().unwrap_or("someone");

        let summary = match event.kind {
            GitHubAttentionKind::Comment => {
                format!(
                    "{actor} commented on {}#{}.",
                    event.repository, event.number
                )
            }
            GitHubAttentionKind::Assignment => format!(
                "{} was assigned to {}#{} by {actor}.",
                event.assignee.as_deref().unwrap_or("someone"),
                event.repository,
                event.number
            ),
            GitHubAttentionKind::SecurityAlert => format!(
                "{} severity security alert raised against {}.",
                event.severity.as_deref().unwrap_or("Unknown"),
                event.repository
            ),
        };

        let link = match event.comment_url.as_deref() {
            Some(url) => format!("\n{url}"),
            None => String::new(),
        };

        match event
            .body
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
        {
            Some(body) if body.chars().count() > MAX_BODY => {
                let truncated: String = body.chars().take(MAX_BODY).collect();
                format!("{summary}{link}\n\n{truncated}…")
            }
            Some(body) => format!("{summary}{link}\n\n{body}"),
            None => format!("{summary}{link}"),
        }
    }

    fn priority(event: &GitHubAttentionEvent) -> i32 {
        match event.kind {
            GitHubAttentionKind::Comment => 2,
            GitHubAttentionKind::Assignment => 3,
            GitHubAttentionKind::SecurityAlert => {
                match event.severity.as_deref().unwrap_or_default() {
                    "critical" | "high" | "error" => 4,
                    "moderate" | "medium" | "warning" => 3,
                    _ => 2,
                }
            }
        }
    }
}

crate::register_job!(GitHubAttentionWorkflow);

impl Job for GitHubAttentionWorkflow {
    type JobType = GitHubAttentionEvent;

    fn partition() -> &'static str {
        "github/attention"
    }

    #[instrument("workflow.github_attention.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let config = services.config();

        let Some(attention) = config.webhooks.github.attention.as_ref() else {
            debug!("Attention handling is not configured; ignoring {job}.");
            return Ok(());
        };

        let filter = match job.kind {
            GitHubAttentionKind::Comment => &attention.comments,
            GitHubAttentionKind::Assignment => &attention.assignments,
            GitHubAttentionKind::SecurityAlert => &attention.security_alerts,
        };

        if !filter.matches(job)? {
            info!("{job} did not match the attention filter; ignoring.");
            return Ok(());
        }

        let unique_key = job.unique_key();

        if job.resolved {
            TodoistCompleteTask::dispatch(
                #[allow(clippy::needless_update)]
                TodoistCompleteTaskPayload {
                    unique_key: unique_key.clone(),
                    config: attention.todoist.clone(),
                    ..Default::default()
                },
                Some(unique_key.into()),
                services,
            )
            .await
        } else {
            TodoistUpsertTask::dispatch(
                TodoistUpsertTaskPayload {
                    unique_key: unique_key.clone(),
                    title: Self::title(job),
                    description: Some(Self::description(job)),
                    due: TodoistDueDate::DateTime(ctx.scheduled_at()),
                    priority: Some(Self::priority(job)),
                    config: attention.todoist.clone(),
                    ..Default::default()
                },
                Some(unique_key.into()),
                services,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhooks::GitHubWebhookConfig;
    use rstest::rstest;

    fn comment(author: &str) -> GitHubAttentionEvent {
        GitHubAttentionEvent {
            kind: GitHubAttentionKind::Comment,
            event: "issue_comment".to_string(),
            action: "created".to_string(),
            resolved: false,
            repository: "example/repo".to_string(),
            repository_owner: "example".to_string(),
            repository_name: "repo".to_string(),
            number: 7,
            title: "Fix the thing".to_string(),
            url: "https://github.com/example/repo/pull/7".to_string(),
            comment_url: Some("https://github.com/example/repo/pull/7#issuecomment-1".to_string()),
            actor: Some(author.to_string()),
            assignee: None,
            subject_author: Some("notheotherben".to_string()),
            body: Some("Any update on this?".to_string()),
            severity: None,
        }
    }

    async fn services_with(
        attention: GitHubAttentionConfig,
    ) -> crate::services::ServicesContainer<crate::db::SqliteDatabase> {
        crate::services::ServicesContainer::new_custom_mock(move |config, _| {
            config.webhooks.github = GitHubWebhookConfig {
                secret: "secret".to_string(),
                auto_merge: None,
                attention: Some(attention),
            };
        })
        .await
        .expect("build mock services")
    }

    async fn upserts<S: Services>(services: &S) -> Vec<String> {
        services
            .queue()
            .peek::<_, serde_json::Value>(TodoistUpsertTask::partition(), 10)
            .await
            .expect("peek the Todoist upsert queue")
            .into_iter()
            .map(|m| m.key)
            .collect()
    }

    #[rstest]
    #[case("notheotherben", true)]
    #[case("dependabot[bot]", false)]
    #[case("dependabot-preview[bot]", false)]
    fn default_comment_filter_excludes_dependabot(#[case] author: &str, #[case] expected: bool) {
        assert_eq!(
            default_comment_filter()
                .matches(&comment(author))
                .expect("the default filter should evaluate"),
            expected
        );
    }

    #[test]
    fn default_assignment_filter_matches_nothing() {
        let mut event = comment("notheotherben");
        event.kind = GitHubAttentionKind::Assignment;
        event.assignee = Some("notheotherben".to_string());

        assert!(
            !default_assignment_filter()
                .matches(&event)
                .expect("the default filter should evaluate")
        );
    }

    #[tokio::test]
    async fn every_comment_on_a_thread_collapses_onto_one_task() {
        let services = services_with(GitHubAttentionConfig::default()).await;

        for author in ["notheotherben", "someone-else"] {
            GitHubAttentionWorkflow
                .handle(
                    JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                    &comment(author),
                )
                .await
                .expect("the comment should raise a task");
        }

        assert_eq!(
            upserts(&services).await,
            vec!["github/attention/example/repo#7".to_string()]
        );
    }

    #[tokio::test]
    async fn filtered_out_comments_raise_nothing() {
        let services = services_with(GitHubAttentionConfig::default()).await;

        GitHubAttentionWorkflow
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &comment("dependabot[bot]"),
            )
            .await
            .expect("a filtered comment should be ignored");

        assert!(upserts(&services).await.is_empty());
    }

    #[tokio::test]
    async fn resolved_alerts_complete_their_task() {
        let services = services_with(GitHubAttentionConfig::default()).await;

        let mut event = comment("dependabot[bot]");
        event.kind = GitHubAttentionKind::SecurityAlert;
        event.event = "dependabot_alert".to_string();
        event.action = "fixed".to_string();
        event.resolved = true;
        event.number = 3;

        GitHubAttentionWorkflow
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &event,
            )
            .await
            .expect("a resolved alert should complete its task");

        assert!(upserts(&services).await.is_empty());

        let completions = services
            .queue()
            .peek::<_, serde_json::Value>(TodoistCompleteTask::partition(), 10)
            .await
            .expect("peek the Todoist completion queue");

        assert_eq!(completions.len(), 1);
        assert_eq!(
            completions[0].key,
            "github/attention/example/repo/dependabot_alert/3"
        );
    }

    #[test]
    fn security_alert_priority_follows_severity() {
        let mut event = comment("dependabot[bot]");
        event.kind = GitHubAttentionKind::SecurityAlert;

        event.severity = Some("critical".to_string());
        assert_eq!(GitHubAttentionWorkflow::priority(&event), 4);

        event.severity = Some("moderate".to_string());
        assert_eq!(GitHubAttentionWorkflow::priority(&event), 3);

        event.severity = Some("low".to_string());
        assert_eq!(GitHubAttentionWorkflow::priority(&event), 2);
    }

    #[test]
    fn long_comment_bodies_are_truncated() {
        let mut event = comment("notheotherben");
        event.body = Some("x".repeat(MAX_BODY * 2));

        let description = GitHubAttentionWorkflow::description(&event);
        assert!(description.ends_with('…'));
        assert!(description.chars().count() < MAX_BODY * 2);
    }
}
