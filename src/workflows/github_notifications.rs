use std::fmt::Display;

use chrono::TimeDelta;
use serde::{Deserialize, Serialize};

use crate::collectors::{
    GitHubNotificationsCollector, GitHubNotificationsSubjectState, GitHubSubjectInformation,
};
use crate::prelude::*;
use crate::publishers::{
    TodoistCompleteTask, TodoistCompleteTaskPayload, TodoistDueDate, TodoistUpsertTask,
    TodoistUpsertTaskPayload,
};
use crate::{config::TodoistConfig, filter::Filter};

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubNotificationsConfig {
    #[serde(default)]
    pub filter: Filter,

    #[serde(default)]
    pub todoist: TodoistConfig,

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

        TodoistUpsertTaskPayload {
            unique_key: event.id.clone(),
            title: format!(
                "[**{}**]({}): {}",
                &event.repository.full_name,
                subject_html_url.unwrap_or(event.repository.html_url.clone()),
                event.subject.title
            ),
            description: Some(
                format!(
                    "Reason: {}\nAuthor: {}",
                    event.reason,
                    subject
                        .map(|s| s.user.login)
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
                if subject.state == GitHubNotificationsSubjectState::Open
                    && subject.user.login == "dependabot[bot]"
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
                        TimeDelta::minutes(30),
                        &services,
                    )
                    .await?;
                } else if subject.state == GitHubNotificationsSubjectState::Open {
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
}

impl Job for GitHubNotificationsWorkflow {
    type JobType = GitHubNotificationsConfig;

    fn partition() -> &'static str {
        "workflow/github-notifications-todoist"
    }

    #[instrument("workflow.github_notifications.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        // Handle delayed auto-close checks
        if let Some(event) = job.event.as_ref() {
            // Check the status of the subject to see if it's still open/active/etc.
            let collector = GitHubNotificationsCollector::new();
            let subject = collector.get_subject(&event.subject, &services).await?;

            match subject {
                None => {
                    TodoistUpsertTask::dispatch(
                        self.build_task(event, job, None),
                        Some(event.id.clone().into()),
                        &services,
                    )
                    .await?
                }
                Some(subject) if subject.state == GitHubNotificationsSubjectState::Open => {
                    TodoistUpsertTask::dispatch(
                        self.build_task(event, job, Some(subject)),
                        Some(event.id.clone().into()),
                        &services,
                    )
                    .await?
                }
                _ => {
                    // Closed/Resolved/Merged/etc., mark as done
                    collector.mark_as_done(&event.id, &services).await?;
                    TodoistCompleteTask::dispatch(
                        #[allow(clippy::needless_update)]
                        TodoistCompleteTaskPayload {
                            unique_key: event.id.clone(),
                            config: job.todoist.clone(),
                            ..Default::default()
                        },
                        Some(event.id.clone().into()),
                        &services,
                    )
                    .await?;
                }
            }

            Ok(())
        } else {
            self.collect_new_notifications(job, services).await
        }
    }
}
