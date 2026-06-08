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

    #[serde(default)]
    pub todoist: TodoistConfig,
}

impl Display for XkcdConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "xkcd")
    }
}

#[derive(Clone)]
pub struct XkcdWorkflow;

crate::register_job!(XkcdWorkflow);

impl Job for XkcdWorkflow {
    type JobType = XkcdConfig;

    fn partition() -> &'static str {
        "xkcd/todoist"
    }

    /// Visibility timeout / retry backoff. Polls the public xkcd feed, so a
    /// failed run waits an hour before retrying to avoid hammering it.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::hours(1)
    }

    #[instrument("workflow.xkcd.setup", skip(self, services), err(Display))]
    async fn setup(
        &self,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let config = services.config();
        CronJob::schedule(&config.workflows.xkcd, services).await
    }

    #[instrument("workflow.xkcd.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let collector = XkcdCollector::new();

        let items = collector.list(services).await?;

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
                services,
            )
            .await?;
        }

        Ok(())
    }
}
