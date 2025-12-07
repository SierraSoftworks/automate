use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::prelude::*;
use crate::publishers::{TodoistCreateTask, TodoistCreateTaskPayload};
use crate::{
    collectors::{Collector, XkcdCollector},
    config::TodoistConfig,
    filter::Filter,
    services::Services,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct XkcdConfig {
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

impl CronWorkflow for XkcdConfig {
    fn schedule(&self) -> croner::Cron {
        self.cron.clone()
    }
}

impl Display for XkcdConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "xkcd")
    }
}

pub struct XkcdToTodoistWorkflow;

impl<S: Services + Clone + Send + Sync + 'static> Job<S> for XkcdToTodoistWorkflow {
    type JobType = XkcdConfig;

    fn partition() -> &'static str {
        "workflow/xkcd-todoist"
    }

    async fn handle(&self, job: &Self::JobType, services: S) -> Result<(), human_errors::Error> {
        let collector = XkcdCollector::new();

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
                    title: format!("[XKCD]({}): {}", &item.url, item.title),
                    description: item.image_url.map(|url| {
                        format!(
                            "![XKCD]({})\n\n*{}*",
                            url,
                            item.image_alt.unwrap_or_default()
                        )
                    }),
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
