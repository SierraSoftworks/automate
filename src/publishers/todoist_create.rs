use crate::{prelude::*, publishers::TodoistClient};
use serde::{Deserialize, Serialize};

use super::TodoistDueDate;

#[derive(Serialize, Deserialize, Default)]
pub struct TodoistCreateTaskPayload {
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<i32>,
    pub due: TodoistDueDate,
    pub duration: Option<chrono::Duration>,
    pub config: crate::config::TodoistConfig,
}

pub struct TodoistCreateTask;

impl Job for TodoistCreateTask {
    type JobType = TodoistCreateTaskPayload;

    fn partition() -> &'static str {
        "todoist/create-task"
    }

    #[instrument(
        "publishers.todoist_create.handle",
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

        client
            .0
            .create_task(&todoist_api::CreateTaskArgs {
                content: TodoistClient::escape_content(&job.title).into_owned(),
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

        Ok(())
    }
}
