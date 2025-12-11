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

impl Job for CalendarWorkflow {
    type JobType = CalendarWorkflowConfig;

    fn partition() -> &'static str {
        "workflow/calendar-todoist"
    }

    #[instrument("workflow.calendar.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let collector = CalendarCollector::new(&job.url);

        let items = collector.diff(&services).await?;

        for item in items.into_iter() {
            match item {
                Diff::Added(id, item) if job.filter.matches(&item).unwrap_or_default() => {
                    info!(
                        "Calendar item '{}' matched filter, creating Todoist task",
                        item.summary
                    );
                    let identifier_string = serde_json::to_string(&id).map_err_as_system(&[
                        "Report this issue to the development team on GitHub.",
                    ])?;
                    crate::publishers::TodoistUpsertTask::dispatch(
                        crate::publishers::TodoistUpsertTaskPayload {
                            unique_key: identifier_string,
                            title: item.summary,
                            description: item.description,
                            priority: job.priority,
                            due: if item.all_day {
                                crate::publishers::TodoistDueDate::Date(item.start.date_naive())
                            } else {
                                crate::publishers::TodoistDueDate::DateTime(item.start.clone())
                            },
                            duration: Some(item.end - item.start),
                            config: job.todoist.clone(),
                        },
                        None,
                        &services,
                    )
                    .await?;
                }
                Diff::Added(id, item) => {
                    info!(
                        "Calendar item '{}' did not match filter, skipping Todoist creation",
                        item.summary
                    );
                    let identifier_string = serde_json::to_string(&id).map_err_as_system(&[
                        "Report this issue to the development team on GitHub.",
                    ])?;
                    crate::publishers::TodoistCompleteTask::dispatch(
                        crate::publishers::TodoistCompleteTaskPayload {
                            unique_key: identifier_string,
                            config: job.todoist.clone(),
                        },
                        None,
                        &services,
                    )
                    .await?;
                }
                Diff::Removed(id) => {
                    let identifier_string = serde_json::to_string(&id).map_err_as_system(&[
                        "Report this issue to the development team on GitHub.",
                    ])?;
                    crate::publishers::TodoistCompleteTask::dispatch(
                        crate::publishers::TodoistCompleteTaskPayload {
                            unique_key: identifier_string,
                            config: job.todoist.clone(),
                        },
                        None,
                        &services,
                    )
                    .await?;
                }
            }
        }

        Ok(())
    }
}
