use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    collectors::YouTubeCollector,
    config::TodoistConfig,
    prelude::*,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct YouTubeConfig {
    pub name: String,
    pub channel_id: String,

    #[serde(default = "default_cron")]
    pub cron: croner::Cron,

    #[serde(default)]
    filter: Filter,

    #[serde(default)]
    pub todoist: TodoistConfig,
}

fn default_cron() -> croner::Cron {
    "@hourly".parse().unwrap()
}

impl CronWorkflow for YouTubeConfig {
    fn schedule(&self) -> croner::Cron {
        self.cron.clone()
    }
}

impl Display for YouTubeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "youtube/{}", self.name)
    }
}

pub struct YouTubeToTodoistWorkflow;

impl<S: Services + Clone + Send + Sync + 'static> Job<S> for YouTubeToTodoistWorkflow {
    type JobType = YouTubeConfig;

    fn partition() -> &'static str {
        "workflow/youtube-todoist"
    }

    async fn handle(&self, job: &Self::JobType, services: S) -> Result<(), human_errors::Error> {
        let collector = YouTubeCollector::new(&job.channel_id);

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
                        "[{}]({}): {}",
                        if item.channel.is_empty() {
                            &job.name
                        } else {
                            &item.channel
                        },
                        item.link,
                        item.title
                    ),
                    due: crate::publishers::TodoistDueDate::Today,
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
