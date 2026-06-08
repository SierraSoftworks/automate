use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    collectors::{CalendarCollector, Diff, DifferentialCollector},
    config::TodoistConfig,
    prelude::*,
};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct CalendarWorkflowConfig {
    pub name: String,
    pub url: String,

    #[serde(default)]
    pub priority: Option<i32>,

    #[serde(default)]
    pub filter: Filter,

    #[serde(default)]
    pub todoist: TodoistConfig,
}

impl Display for CalendarWorkflowConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "calendar/{}", self.name)
    }
}

#[derive(Clone)]
pub struct CalendarWorkflow;

crate::register_job!(CalendarWorkflow);

impl Job for CalendarWorkflow {
    type JobType = CalendarWorkflowConfig;

    fn partition() -> &'static str {
        "calendar/todoist"
    }

    /// Visibility timeout / retry backoff. Calendar sync is cheap and not
    /// heavily rate limited, so a failed run can be retried promptly.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::minutes(5)
    }

    #[instrument("workflow.calendar.setup", skip(self, services), err(Display))]
    async fn setup(
        &self,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let config = services.config();
        CronJob::schedule(&config.workflows.calendars, services).await
    }

    #[instrument("workflow.calendar.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let collector = CalendarCollector::new(&job.url);

        let items = collector.diff(services).await?;

        for item in items.into_iter() {
            match item {
                Diff::Added(id, item) | Diff::Modified(id, item)
                    if job.filter.matches(&item).unwrap_or_default() =>
                {
                    info!(
                        "Calendar item '{}' matched filter, creating Todoist task",
                        item.summary
                    );
                    let identifier_string = serde_json::to_string(&id)
                        .or_system_err(&["Report this issue to the development team on GitHub."])?;
                    crate::publishers::TodoistUpsertTask::dispatch(
                        crate::publishers::TodoistUpsertTaskPayload {
                            unique_key: identifier_string,
                            title: item.summary,
                            description: item.description,
                            priority: job.priority,
                            due: if item.all_day {
                                crate::publishers::TodoistDueDate::Date(item.start.date_naive())
                            } else {
                                crate::publishers::TodoistDueDate::DateTime(item.start)
                            },
                            duration: Some(item.end - item.start),
                            config: job.todoist.clone(),
                        },
                        None,
                        services,
                    )
                    .await?;
                }
                Diff::Added(id, item) | Diff::Modified(id, item) => {
                    info!(
                        "Calendar item '{}' did not match filter, skipping Todoist creation",
                        item.summary
                    );
                    let identifier_string = serde_json::to_string(&id)
                        .or_system_err(&["Report this issue to the development team on GitHub."])?;
                    crate::publishers::TodoistCompleteTask::dispatch(
                        crate::publishers::TodoistCompleteTaskPayload {
                            unique_key: identifier_string,
                            config: job.todoist.clone(),
                        },
                        None,
                        services,
                    )
                    .await?;
                }
                Diff::Removed(id) => {
                    let identifier_string = serde_json::to_string(&id)
                        .or_system_err(&["Report this issue to the development team on GitHub."])?;
                    crate::publishers::TodoistCompleteTask::dispatch(
                        crate::publishers::TodoistCompleteTaskPayload {
                            unique_key: identifier_string,
                            config: job.todoist.clone(),
                        },
                        None,
                        services,
                    )
                    .await?;
                }
            }
        }

        Ok(())
    }
}
