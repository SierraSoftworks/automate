use crate::prelude::*;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use todoist_api::{CreateTaskArgs, TodoistWrapper, UpdateTaskArgs};

#[derive(Serialize, Deserialize, Default)]
pub struct TodoistCreateTaskPayload {
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<i32>,
    pub due: TodoistDueDate,
    pub duration: Option<chrono::Duration>,
    pub config: crate::config::TodoistConfig,
}

#[derive(Serialize, Deserialize, Default)]
pub enum TodoistDueDate {
    #[default]
    None,
    Today,
    Date(chrono::NaiveDate),
    DateTime(chrono::DateTime<chrono::Utc>),
}

impl TodoistDueDate {
    pub fn due_date(&self) -> Option<String> {
        if let TodoistDueDate::Date(date) = self {
            Some(date.format("%Y-%m-%d").to_string())
        } else {
            None
        }
    }

    pub fn due_datetime(&self) -> Option<String> {
        if let TodoistDueDate::DateTime(datetime) = self {
            Some(datetime.to_rfc3339())
        } else {
            None
        }
    }

    pub fn due_string(&self) -> Option<String> {
        if let TodoistDueDate::Today = self {
            Some("today".into())
        } else {
            None
        }
    }
}

pub struct TodoistCreateTask;

impl Job for TodoistCreateTask {
    type JobType = TodoistCreateTaskPayload;

    fn partition() -> &'static str {
        "todoist/create-task"
    }

    async fn handle(&self, job: &Self::JobType, services: impl Services + Send + Sync + 'static) -> Result<(), human_errors::Error> {
        let config = services.config().connections.todoist.merge(&job.config);

        let client = get_client(&config)?;

        let project_id = get_project_id(
            config.project.as_deref().unwrap_or("Inbox"),
            &services,
            client.clone(),
        )
        .await?;
        let section_id = get_section_id(
            config.project.as_deref().unwrap_or("Inbox"),
            &project_id,
            config.section.as_deref(),
            &services,
            client.clone(),
        )
        .await?;

        client
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
            .wrap_err_as_user(
                format!("Failed to create Todoist task '{}'.", job.title),
                &[
                    "Check that your Todoist API token is valid and has the necessary permissions.",
                    "Ensure that you have specified the correct Todoist project and section names.",
                ],
            )?;

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

fn get_client(
    config: &crate::config::TodoistConfig,
) -> Result<Arc<TodoistWrapper>, human_errors::Error> {
    let api_token = config.api_key.clone().ok_or_else(|| {
        human_errors::user("You have not provided a Todoist API key.", &[
            "Set the 'connection.todoist.api_key' secret to a valid Todoist API token to enable publishing.",
        ])
    })?;

    Ok(Arc::new(TodoistWrapper::new(api_token)))
}

async fn get_project_id(
    name: &str,
    services: &impl crate::services::Services,
    client: Arc<TodoistWrapper>,
) -> Result<String, human_errors::Error> {
    let partition = "todoist/projects";
    let key = "default";

    let projects = services
        .cache()
        .cached(
            partition,
            key,
            move || {
                Box::pin(async move {
                    client.get_projects().await.wrap_err_as_user(
            "Failed to fetch Todoist projects.",
            &[
                "Check that your Todoist API token is valid and has the necessary permissions.",
            ],
        )
                })
            },
            chrono::Duration::hours(24),
        )
        .await?;

    let project = projects
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| {
            human_errors::user(
                format!("Todoist project '{}' not found.", name),
                &["Ensure that the specified project name is correct."],
            )
        })?;

    Ok(project.id)
}

async fn get_section_id(
    project_name: &str,
    project_id: &str,
    name: Option<&str>,
    services: &impl crate::services::Services,
    client: Arc<TodoistWrapper>,
) -> Result<Option<String>, human_errors::Error> {
    if let Some(section_name) = name {
        let partition = "todoist/sections";
        let key = "default";

        let sections = services
            .cache()
            .cached(
                partition,
                key,
                move || {
                    Box::pin(async move {
                        client.get_sections().await.wrap_err_as_user(
                "Failed to fetch Todoist sections.",
                &[
                    "Check that your Todoist API token is valid and has the necessary permissions.",
                ],
            )
                    })
                },
                chrono::Duration::hours(24),
            )
            .await?;

        let section = sections
            .into_iter()
            .find(|s| s.project_id == project_id && s.name == *section_name)
            .ok_or_else(|| {
                human_errors::user(
                    format!(
                        "Todoist section '{}' not found in project '{}'.",
                        section_name, project_name
                    ),
                    &["Ensure that the specified section name is correct."],
                )
            })?;

        Ok(Some(section.id))
    } else {
        Ok(None)
    }
}
