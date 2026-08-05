use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::prelude::*;
use crate::publishers::{TodoistCreateTask, TodoistCreateTaskPayload};
use crate::{
    collectors::{Collector, XkcdCollector},
    filter::Filter,
    publishers::TodoistTarget,
    services::Services,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct XkcdConfig {
    #[serde(default)]
    pub filter: Filter,

    #[serde(default)]
    pub todoist: TodoistTarget,
}

impl Display for XkcdConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "xkcd")
    }
}

#[derive(Clone)]
pub struct XkcdWorkflow;

crate::register_job!(XkcdWorkflow);
crate::register_workflow_type!(XkcdWorkflow);

impl crate::workflows::ConfigurableWorkflow for XkcdWorkflow {
    type ConfigType = XkcdConfig;

    fn type_id() -> &'static str {
        "xkcd"
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: <Self as crate::workflows::ConfigurableWorkflow>::type_id().to_string(),
            name: "XKCD".to_string(),
            description: "Files a task for each new XKCD comic.".to_string(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "@daily".to_string(),
            },
            fields: [FieldDescriptor::new(
                crate::config_path!(XkcdConfig: filter),
                "Filter",
                FieldKind::Filter {
                    fields: vec!["title".into(), "url".into(), "has_image".into()],
                },
            )
            .with_help("Only file comics matching this. Leave it empty to file every comic.")]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                XkcdConfig,
                project = Some("Hobbies"),
                section = Some("Reading")
            ))
            .collect(),
        }
    }
}

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
