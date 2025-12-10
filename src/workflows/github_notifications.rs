use std::fmt::Display;

use chrono::TimeDelta;
use serde::{Deserialize, Serialize};

use crate::collectors::GitHubNotificationsCollector;
use crate::prelude::*;
use crate::publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate};
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

impl Job for GitHubNotificationsWorkflow {
    type JobType = GitHubNotificationsConfig;

    fn partition() -> &'static str {
        "workflow/github-notifications-todoist"
    }

    async fn handle(&self, job: &Self::JobType, services: impl Services + Send + Sync + 'static) -> Result<(), human_errors::Error> {
        if let Some(event) = job.event.as_ref() {

            // Check the status of the subject to see if it's still open/active/etc.
            let collector = GitHubNotificationsCollector::new();
            let subject_state = collector.get_subject_state(&event.subject, &services).await?;

            match subject_state {
                crate::collectors::GitHubNotificationsSubjectState::Open => {
                    // Still open, create a Todoist task for it (since it's not being automatically resolved)
                    let subject_html_url = event.subject.url.as_ref().map(|url| {
                        url.replace("api.github.com/repos/", "github.com/").replace("/pulls/", "/pull/")
                    });

                    TodoistCreateTask::dispatch(
                        TodoistCreateTaskPayload {
                            title: format!(
                                "[github:{}]({}): [{}]({}) ({})",
                                &event.repository.full_name, &event.repository.html_url, event.subject.title, subject_html_url.unwrap_or_default(), serde_json::to_string(&event.reason).unwrap_or_default()
                            ),
                            due: TodoistDueDate::DateTime(event.updated_at),
                            config: job.todoist.clone(),
                            priority: Some(event.reason.priority()),
                            ..Default::default()
                        },
                        None,
                        &services,
                    )
                    .await?;
                }
                _ => {
                    // Closed/Resolved/Merged/etc., mark as done
                    collector.mark_as_done(&event.id, &services).await?;
                }
            }


            

            Ok(())

        } else {
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

                let id = item.id.clone();
                Self::dispatch_delayed(GitHubNotificationsConfig {
                    event: Some(item),
                    filter: job.filter.clone(),
                    todoist: job.todoist.clone(),
                }, Some(id.into()), TimeDelta::hours(1), &services).await?;
            }
            Ok(())
        }
    }
}
