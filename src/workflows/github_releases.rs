use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::prelude::*;
use crate::publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate};
use crate::{collectors::GitHubReleasesCollector, config::TodoistConfig, filter::Filter};

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubReleasesConfig {
    pub repository: String,

    #[serde(default)]
    pub filter: Filter,

    #[serde(default)]
    pub todoist: TodoistConfig,
}

impl Display for GitHubReleasesConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "github-releases/{}", self.repository)
    }
}

#[derive(Clone)]
pub struct GitHubReleasesWorkflow;

impl Job for GitHubReleasesWorkflow {
    type JobType = GitHubReleasesConfig;

    fn partition() -> &'static str {
        "workflow/github-releases-todoist"
    }

    #[instrument("workflow.github_releases.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(&self, job: &Self::JobType, services: impl Services + Send + Sync + 'static) -> Result<(), human_errors::Error> {
        let collector = GitHubReleasesCollector::new(&job.repository);

        let items = collector.list(&services).await?;

        for item in items.into_iter() {
            match job.filter.matches(&item) {
                Ok(false) => continue,
                Err(err) => {
                    return Err(err);
                }
                _ => {}
            }

            TodoistCreateTask::dispatch(
                TodoistCreateTaskPayload {
                    title: format!(
                        "[github:{}]({}): Released {} ({})",
                        &job.repository, &item.html_url, item.name, item.tag_name
                    ),
                    description: item.body.map(|body| {
                        crate::parsers::html_to_markdown(
                            &body,
                            "https://github.com/".parse().unwrap(),
                        )
                    }),
                    due: TodoistDueDate::Today,
                    config: job.todoist.clone(),
                    ..Default::default()
                },
                None,
                &services,
            )
            .await?;
        }

        Ok(())
    }
}
