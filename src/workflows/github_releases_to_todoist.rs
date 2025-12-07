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

    #[serde(default = "default_cron")]
    pub cron: croner::Cron,

    #[serde(default)]
    pub todoist: TodoistConfig,
}

fn default_cron() -> croner::Cron {
    "@hourly".parse().unwrap()
}

impl Display for GitHubReleasesConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "github-releases/{}", self.repository)
    }
}

impl CronWorkflow for GitHubReleasesConfig {
    fn schedule(&self) -> croner::Cron {
        self.cron.clone()
    }
}

pub struct GitHubReleasesToTodoistWorkflow;

impl<S: Services + Clone + Send + Sync + 'static> Job<S> for GitHubReleasesToTodoistWorkflow {
    type JobType = GitHubReleasesConfig;

    fn partition() -> &'static str {
        "workflow/github-releases-todoist"
    }

    async fn handle(&self, job: &Self::JobType, services: S) -> Result<(), human_errors::Error> {
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
