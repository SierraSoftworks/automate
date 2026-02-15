use crate::{prelude::*, publishers::TodoistClient};
use serde::{Deserialize, Serialize};

use super::TodoistDueDate;

#[derive(Serialize, Deserialize, Default)]
pub struct TodoistUpsertTaskPayload {
    pub unique_key: String,

    pub title: String,
    pub description: Option<String>,
    pub priority: Option<i32>,
    pub due: TodoistDueDate,
    pub duration: Option<chrono::Duration>,
    pub config: crate::config::TodoistConfig,
}

pub struct TodoistUpsertTask;

#[derive(Serialize, Deserialize)]
pub struct TodoistUpsertTaskState {
    pub id: String,
    pub hash: String,
    pub title: Option<String>,
}

impl Job for TodoistUpsertTask {
    type JobType = TodoistUpsertTaskPayload;

    fn partition() -> &'static str {
        "todoist/upsert-task"
    }

    #[instrument(
        "publishers.todoist_upsert.handle",
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
        let hash = self.job_hash(job)?;

        if let Some(existing_task) = services
            .kv()
            .get::<TodoistUpsertTaskState>("todoist/task", job.unique_key.clone())
            .await?
        {
            if hash == existing_task.hash {
                // No changes, skip update
                return Ok(());
            }

            let task = client.0.update_task(&existing_task.id, &todoist_api::UpdateTaskArgs {
                content: Some(TodoistClient::escape_content(&job.title).into_owned()),
                description: job.description.clone(),
                due_date: job.due.due_date(),
                due_datetime: job.due.due_datetime(),
                due_string: job.due.due_string(),
                due_lang: Some("en".into()),
                duration: job.duration.map(|d| d.num_minutes() as i32),
                duration_unit: job.duration.map(|_| "minute".into()),
                priority: job.priority,
                ..Default::default()
            }).await.wrap_user_err(
                format!("Failed to update Todoist task '{}'.", job.title),
                &[
                    "Check that your Todoist API token is valid and has the necessary permissions.",
                    "Ensure that you have specified the correct Todoist project and section names.",
                ],
            )?;

            if task.completed_at.is_some() {
                client.0.reopen_task(&existing_task.id).await.wrap_user_err(
                    format!("Failed to reopen completed Todoist task '{}'.", job.title),
                    &[
                        "Check that your Todoist API token is valid and has the necessary permissions.",
                        "Ensure that you have specified the correct Todoist project and section names.",
                    ],
                )?;
            }

            services
                .kv()
                .set(
                    "todoist/task",
                    job.unique_key.clone(),
                    TodoistUpsertTaskState {
                        id: existing_task.id.clone(),
                        hash,
                        title: Some(job.title.clone()),
                    },
                )
                .await?;
        } else {
            let project_id = client
                .get_project_id(config.project.as_deref().unwrap_or("Inbox"), &services)
                .await?;
            let section_id = client
                .get_section_id(
                    config.project.as_deref().unwrap_or("Inbox"),
                    &project_id,
                    config.section.as_deref(),
                    &services,
                )
                .await?;

            let task = client.0
                .create_task(&todoist_api::CreateTaskArgs {
                    content: job.title.clone(),
                    description: job.description.clone(),
                    due_date: job.due.due_date(),
                    due_datetime: job.due.due_datetime(),
                    due_string: job.due.due_string(),
                    due_lang: Some("en".into()),
                    duration: job.duration.map(|d| d.num_minutes() as i32),
                    duration_unit: job.duration.map(|_| "minute".into()),
                    project_id: Some(project_id),
                    section_id,
                    priority: job.priority,
                    ..Default::default()
                })
                .await
                .wrap_user_err(
                    format!("Failed to create Todoist task '{}'.", job.title),
                    &[
                        "Check that your Todoist API token is valid and has the necessary permissions.",
                        "Ensure that you have specified the correct Todoist project and section names.",
                    ],
                )?;

            services
                .kv()
                .set(
                    "todoist/task",
                    job.unique_key.clone(),
                    TodoistUpsertTaskState {
                        id: task.id.clone(),
                        hash,
                        title: Some(job.title.clone()),
                    },
                )
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
