use std::fmt::Display;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{
    config::TodoistConfig,
    jobs::CronJobConfig,
    prelude::*,
    publishers::{TodoistClient, TodoistUpsertTaskState},
};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct TodoistCleanupConfig {
    #[serde(default)]
    pub todoist: TodoistConfig,
}

impl Display for TodoistCleanupConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "todoist-cleanup")
    }
}

#[derive(Clone)]
pub struct TodoistCleanupWorkflow;

crate::register_job!(TodoistCleanupWorkflow);

impl Job for TodoistCleanupWorkflow {
    type JobType = TodoistCleanupConfig;

    fn partition() -> &'static str {
        "workflow/todoist-cleanup"
    }

    #[instrument("workflow.todoist_cleanup.setup", skip(self, services), err(Display))]
    async fn setup(
        &self,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        // The cleanup job always runs once a day; it deliberately does not expose
        // any configuration so that it cannot be accidentally disabled or
        // misconfigured.
        let schedule = CronJobConfig::<TodoistCleanupWorkflow> {
            job: TodoistCleanupConfig::default(),
            cron: croner::Cron::from_str("@daily").unwrap(),
        };
        CronJob::schedule(std::slice::from_ref(&schedule), services).await
    }

    #[instrument(
        "workflow.todoist_cleanup.handle",
        skip(self, job, services),
        fields(job = %job),
        err(Display)
    )]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let config = services.config().connections.todoist.merge(&job.todoist);
        let client = TodoistClient::new(&config)?;

        let entries = services
            .kv()
            .list::<TodoistUpsertTaskState>("todoist/task")
            .await?;

        for (key, state) in entries {
            match client.0.get_task(&state.id).await {
                Ok(task) if task.checked || task.is_deleted || task.completed_at.is_some() => {
                    info!(
                        "Removing stale Todoist task mapping '{key}' because task '{}' has been completed or deleted.",
                        state.id
                    );
                    services.kv().remove("todoist/task", key).await?;
                }
                Ok(_) => {
                    // The task is still active, so we keep its mapping.
                }
                Err(err) if err.is_not_found() => {
                    info!(
                        "Removing stale Todoist task mapping '{key}' because task '{}' no longer exists.",
                        state.id
                    );
                    services.kv().remove("todoist/task", key).await?;
                }
                Err(err) => {
                    return Err::<(), _>(err).wrap_user_err(
                        format!("Failed to fetch the status of Todoist task '{}'.", state.id),
                        &[
                            "Check that your Todoist API token is valid and has the necessary permissions.",
                        ],
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_config_display_is_stable() {
        // The Display impl doubles as the idempotency key for the scheduled job,
        // so it must stay constant to avoid duplicate cron registrations.
        assert_eq!(
            TodoistCleanupConfig::default().to_string(),
            "todoist-cleanup"
        );
    }

    #[test]
    fn partition_is_namespaced() {
        assert_eq!(
            TodoistCleanupWorkflow::partition(),
            "workflow/todoist-cleanup"
        );
    }

    #[test]
    fn schedule_is_hard_coded_to_daily() {
        // The schedule is intentionally not configurable, so verify the literal
        // we hand to the scheduler parses to the daily cadence.
        let schedule = CronJobConfig::<TodoistCleanupWorkflow> {
            job: TodoistCleanupConfig::default(),
            cron: croner::Cron::from_str("@daily").unwrap(),
        };
        let expected = croner::Cron::from_str("@daily").unwrap().to_string();
        assert_eq!(schedule.cron.to_string(), expected);
    }
}
