use std::fmt::Display;

use chrono::TimeDelta;
use serde::{Deserialize, Serialize};

use crate::collectors::{GitHubNotificationsCollector, GitHubSubjectInformation};
use crate::jobs::subject_key;
use crate::prelude::*;
use crate::publishers::{
    TodoistCompleteTask, TodoistCompleteTaskPayload, TodoistDueDate, TodoistUpsertTask,
    TodoistUpsertTaskPayload,
};
use crate::{filter::Filter, publishers::TodoistTarget};

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubNotificationsConfig {
    #[serde(default)]
    pub filter: Filter,

    #[serde(default)]
    pub todoist: TodoistTarget,

    event: Option<<GitHubNotificationsCollector as Collector>::Item>,
}

impl Display for GitHubNotificationsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "github-notifications")
    }
}

#[derive(Clone)]
pub struct GitHubNotificationsWorkflow;

impl GitHubNotificationsWorkflow {
    /// The Todoist key tracking this notification's subject.
    ///
    /// Issues and pull requests are keyed on `{repo}#{number}` so that the
    /// webhook-driven attention path resolves to the same task. Their API URL
    /// cannot serve as the key because GitHub spells the same pull request as
    /// `pulls/{n}` in a notification and `issues/{n}` in an `issue_comment`
    /// payload; the number is shared between both.
    ///
    /// Everything else (releases, check suites) has no webhook counterpart to
    /// converge with, so its own API URL is the natural identity. Only subjects
    /// carrying no URL at all fall back to the notification thread id, which is
    /// a last resort because marking a thread done ends it and later activity
    /// arrives under a new one.
    fn task_key(event: &<GitHubNotificationsCollector as Collector>::Item) -> String {
        match event.subject.issue_reference() {
            Some((repository, number)) => subject_key(&repository, number),
            None => event
                .subject
                .url
                .clone()
                .unwrap_or_else(|| event.id.clone()),
        }
    }

    fn build_task(
        &self,
        event: &<GitHubNotificationsCollector as Collector>::Item,
        job: &GitHubNotificationsConfig,
        subject: Option<GitHubSubjectInformation>,
    ) -> TodoistUpsertTaskPayload {
        // Still open, create a Todoist task for it (since it's not being automatically resolved)
        let subject_html_url = event.subject.url.as_ref().map(|url| {
            url.replace("api.github.com/repos/", "github.com/")
                .replace("/pulls/", "/pull/")
        });

        // Rendered identically to the attention path's title, so that a subject
        // seen by both does not flip between two spellings of the same task.
        let reference = match event.subject.issue_reference() {
            Some((repository, number)) => format!("{repository}#{number}"),
            None => event.repository.full_name.clone(),
        };

        TodoistUpsertTaskPayload {
            unique_key: Self::task_key(event),
            title: format!(
                "[**{}**]({}): {}",
                reference,
                subject_html_url.unwrap_or(event.repository.html_url.clone()),
                event.subject.title
            ),
            description: Some(
                format!(
                    "Reason: {}\nAuthor: {}",
                    event.reason,
                    subject
                        .and_then(|s| s.user.map(|u| u.login))
                        .unwrap_or("unknown".to_string()),
                )
                .trim()
                .to_string(),
            ),
            due: TodoistDueDate::DateTime(event.updated_at),
            config: job.todoist.clone(),
            priority: Some(event.reason.priority()),
            ..Default::default()
        }
    }

    async fn collect_new_notifications(
        &self,
        job: &GitHubNotificationsConfig,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let collector = GitHubNotificationsCollector::new();
        let items = collector.list(&services).await?;

        for item in items.into_iter() {
            match job.filter.matches(&item) {
                Ok(false) => continue,
                Err(err) => {
                    return Err(err);
                }
                _ => {}
            }

            if let Some(subject) = collector.get_subject(&item.subject, &services).await? {
                if !subject.is_open() {
                    // Closing the subject bumps its notification thread, so a
                    // webhook-triggered collection resolves it within seconds
                    // rather than waiting for the delayed re-check below.
                    self.resolve(&collector, &item, job, &services).await?;
                } else if subject
                    .user
                    .as_ref()
                    .is_some_and(|u| u.login == "dependabot[bot]")
                {
                    // Schedule an auto-close task to resolve this notification later if the PR is auto-merged

                    let id = item.id.clone();
                    Self::dispatch_delayed(
                        GitHubNotificationsConfig {
                            event: Some(item),
                            filter: job.filter.clone(),
                            todoist: job.todoist.clone(),
                        },
                        Some(id.into()),
                        TimeDelta::hours(2),
                        &services,
                    )
                    .await?;
                } else {
                    TodoistUpsertTask::dispatch(
                        self.build_task(&item, job, Some(subject)),
                        Some(item.id.clone().into()),
                        &services,
                    )
                    .await?;
                }
            } else {
                TodoistUpsertTask::dispatch(
                    self.build_task(&item, job, None),
                    Some(item.id.clone().into()),
                    &services,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Marks a resolved notification thread as done and completes the Todoist
    /// task which was tracking it.
    async fn resolve(
        &self,
        collector: &GitHubNotificationsCollector,
        event: &<GitHubNotificationsCollector as Collector>::Item,
        job: &GitHubNotificationsConfig,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<(), human_errors::Error> {
        collector.mark_as_done(&event.id, services).await?;

        TodoistCompleteTask::dispatch(
            #[allow(clippy::needless_update)]
            TodoistCompleteTaskPayload {
                unique_key: Self::task_key(event),
                config: job.todoist.clone(),
                ..Default::default()
            },
            Some(event.id.clone().into()),
            services,
        )
        .await
    }
}

crate::register_job!(GitHubNotificationsWorkflow);

impl Job for GitHubNotificationsWorkflow {
    type JobType = GitHubNotificationsConfig;

    fn partition() -> &'static str {
        "github/notifications/todoist"
    }

    /// Visibility timeout / retry backoff. Calls the rate-limited GitHub API, so
    /// a failed run waits an hour before retrying.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::hours(1)
    }

    #[instrument(
        "workflow.github_notifications.setup",
        skip(self, services),
        err(Display)
    )]
    async fn setup(
        &self,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let config = services.config();
        CronJob::schedule(&config.workflows.github_notifications, services).await
    }

    #[instrument("workflow.github_notifications.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        // Handle delayed auto-close checks
        if let Some(event) = job.event.as_ref() {
            let services = ctx.services();

            // Check the status of the subject to see if it's still open/active/etc.
            let collector = GitHubNotificationsCollector::new();
            let subject = collector.get_subject(&event.subject, services).await?;

            match subject {
                None => {
                    TodoistUpsertTask::dispatch(
                        self.build_task(event, job, None),
                        Some(event.id.clone().into()),
                        services,
                    )
                    .await?
                }
                Some(subject) if subject.is_open() => {
                    TodoistUpsertTask::dispatch(
                        self.build_task(event, job, Some(subject)),
                        Some(event.id.clone().into()),
                        services,
                    )
                    .await?
                }
                _ => {
                    // Closed/Resolved/Merged/etc., mark as done
                    self.resolve(&collector, event, job, services).await?;
                }
            }

            Ok(())
        } else {
            self.collect_new_notifications(job, ctx.into_services())
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhooks::GitHubAttentionEvent;

    fn notification(
        subject_url: Option<&str>,
    ) -> <GitHubNotificationsCollector as Collector>::Item {
        serde_json::from_value(serde_json::json!({
            "id": "14523698",
            "reason": "review_requested",
            "unread": true,
            "updated_at": "2026-07-31T12:00:00Z",
            "last_read_at": null,
            "repository": {
                "name": "repo",
                "full_name": "example/repo",
                "html_url": "https://github.com/example/repo",
                "owner": { "login": "example", "html_url": "https://github.com/example" },
            },
            "subject": {
                "title": "Bump serde",
                "type": "PullRequest",
                "url": subject_url,
                "latest_comment_url": null,
            },
        }))
        .expect("the sample notification should deserialize")
    }

    fn comment_on(repository: &str, number: u64) -> GitHubAttentionEvent {
        serde_json::from_value(serde_json::json!({
            "kind": "comment",
            "event": "issue_comment",
            "action": "created",
            "resolved": false,
            "repository": repository,
            "repository_owner": "example",
            "repository_name": "repo",
            "number": number,
            "title": "Bump serde",
            "url": format!("https://github.com/{repository}/pull/{number}"),
            "actor": "someone-else",
            "assignee": null,
            "subject_author": "notheotherben",
            "body": "Any update?",
            "severity": null,
        }))
        .expect("the sample attention event should deserialize")
    }

    /// The regression this guards: keying on the notification thread id meant a
    /// webhook comment and the notification about it resolved to different
    /// Todoist tasks, so one pull request accumulated several entries.
    #[test]
    fn a_notification_and_a_webhook_comment_share_one_task() {
        let event = notification(Some("https://api.github.com/repos/example/repo/pulls/7"));

        assert_eq!(
            GitHubNotificationsWorkflow::task_key(&event),
            comment_on("example/repo", 7).unique_key()
        );
    }

    #[test]
    fn subjects_without_an_issue_are_keyed_on_their_api_url() {
        // A release has no webhook counterpart to converge with, but its URL is
        // still a stabler identity than the notification thread id.
        let event = notification(Some("https://api.github.com/repos/example/repo/releases/1"));

        assert_eq!(
            GitHubNotificationsWorkflow::task_key(&event),
            "https://api.github.com/repos/example/repo/releases/1"
        );
    }

    #[test]
    fn subjects_without_a_url_fall_back_to_the_thread_id() {
        assert_eq!(
            GitHubNotificationsWorkflow::task_key(&notification(None)),
            "14523698"
        );
    }
}
