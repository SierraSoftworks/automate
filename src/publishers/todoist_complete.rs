use crate::{prelude::*, publishers::TodoistClient};
use serde::{Deserialize, Serialize};

use super::todoist_upsert::TodoistUpsertTaskState;

#[derive(Serialize, Deserialize, Default)]
pub struct TodoistCompleteTaskPayload {
    pub unique_key: String,

    pub config: crate::config::TodoistConfig,
}

pub struct TodoistCompleteTask;

impl Job for TodoistCompleteTask {
    type JobType = TodoistCompleteTaskPayload;

    fn partition() -> &'static str {
        "todoist/complete-task"
    }

    #[instrument(
        "publishers.todoist_complete.handle",
        skip(self, job, services),
        err(Display)
    )]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let config = services.config().connections.todoist.merge(&job.config);

        let client = TodoistClient::new(&config)?;

        if let Some(existing_task) = services
            .kv()
            .get::<TodoistUpsertTaskState>("todoist/task", job.unique_key.clone())
            .await?
        {
            client.0.complete_task(&existing_task.id).await.wrap_err_as_user(
                format!("Failed to complete Todoist task '{}'.", &existing_task.id),
                &[
                    "Check that your Todoist API token is valid and has the necessary permissions.",
                    "Ensure that you have specified the correct Todoist project and section names.",
                ],
            )?;

            services
                .kv()
                .remove("todoist/task", job.unique_key.clone())
                .await?;
        }

        Ok(())
    }
}

// async fn update(task_id: &str, update: UpdateTaskArgs, config: &TodoistConfig, _services: &impl crate::services::Services) -> Result<(), human_errors::Error> {
//     let client = get_client(config)?;

//     client.update_task(task_id, &update).await.wrap_err_as_user(
//         "Failed to update Todoist task.",
//         &[
//             "Check that your Todoist API token is valid and has the necessary permissions.",
//             "Ensure that the task ID is correct.",
//         ],
//     )?;

//     Ok(())
// }

// async fn complete(task_id: &str, config: &TodoistConfig, _services: &impl crate::services::Services) -> Result<(), human_errors::Error> {
//     let client = get_client(config)?;

//     client.complete_task(task_id).await.wrap_err_as_user(
//         "Failed to complete Todoist task.",
//         &[
//             "Check that your Todoist API token is valid and has the necessary permissions.",
//             "Ensure that the task ID is correct.",
//         ],
//     )?;

//     Ok(())
// }
