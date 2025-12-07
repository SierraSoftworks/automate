use std::{borrow::Cow, sync::Arc};

use human_errors::ResultExt;
use serde::Deserialize;
use todoist_api::{TodoistWrapper, CreateTaskArgs, UpdateTaskArgs};

use crate::{db::Cache, publishers::{Publisher, PublisherConfig}};

pub struct TodoistPublisher;

#[derive(Clone, Deserialize, Default)]
pub struct TodoistConfig {
    pub api_key: Option<String>,
    pub project: Option<String>,
    pub section: Option<String>,
    pub priority: Option<i32>,
}

impl PublisherConfig for TodoistConfig {
    fn merge(&self, other: &Self) -> Self {
        TodoistConfig {
            api_key: other.api_key.clone().or_else(|| self.api_key.clone()),
            project: other.project.clone().or_else(|| self.project.clone()),
            section: other.section.clone().or_else(|| self.section.clone()),
            priority: other.priority.or(self.priority),
        }
    }
}

impl Publisher for TodoistPublisher {
    type Item = CreateTaskArgs;
    type Config = TodoistConfig;

    fn kind(&self) -> Cow<'static, str> {
        Cow::Borrowed("todoist")
    }
    
    fn key(&self) -> Cow<'static, str> {
        Cow::Borrowed("0")
    }
    
    async fn publish(&self, mut item: Self::Item, config: Self::Config, services: &impl crate::services::Services) -> Result<(), human_errors::Error> {
        let client = self.get_client(&config)?;

        let project_name = config.project.as_deref().unwrap_or("Inbox");

        let project_id = self.get_project_id(&project_name, services, client.clone()).await?;
        let section_id = self.get_section_id(&project_name, &project_id, config.section.as_deref(), services, client.clone()).await?;

        item.project_id = Some(project_id);
        if let Some(section_id) = section_id {
            item.section_id = Some(section_id);
        }

        if let Some(priority) = config.priority {
            item.priority = Some(priority);
        }

        client.create_task(&item).await.wrap_err_as_user(
            "Failed to create Todoist task.",
            &[
                "Check that your Todoist API token is valid and has the necessary permissions.",
                "Ensure that you have specified the correct Todoist project and section names.",
            ],
        )?;

        Ok(())
    }
}

impl TodoistPublisher {
    pub async fn update(&self, task_id: &str, update: UpdateTaskArgs, config: &TodoistConfig, services: &impl crate::services::Services) -> Result<(), human_errors::Error> {
        let client = self.get_client(config)?;

        client.update_task(task_id, &update).await.wrap_err_as_user(
            "Failed to update Todoist task.",
            &[
                "Check that your Todoist API token is valid and has the necessary permissions.",
                "Ensure that the task ID is correct.",
            ],
        )?;

        Ok(())
    }

    pub async fn complete(&self, task_id: &str, config: &TodoistConfig, services: &impl crate::services::Services) -> Result<(), human_errors::Error> {
        let client = self.get_client(config)?;

        client.complete_task(task_id).await.wrap_err_as_user(
            "Failed to complete Todoist task.",
            &[
                "Check that your Todoist API token is valid and has the necessary permissions.",
                "Ensure that the task ID is correct.",
            ],
        )?;

        Ok(())
    }

    fn get_client(&self, config: &TodoistConfig) -> Result<Arc<TodoistWrapper>, human_errors::Error> {
        let api_token = config.api_key.clone().ok_or_else(|| {
            human_errors::user("You have not provided a Todoist API key.", &[
                "Set the 'connection.todoist.api_key' secret to a valid Todoist API token to enable publishing.",
            ])
        })?;
        
        Ok(Arc::new(TodoistWrapper::new(api_token)))
    }

    async fn get_project_id(&self, name: &str, services: &impl crate::services::Services, client: Arc<TodoistWrapper>) -> Result<String, human_errors::Error> {
        let partition = self.partition(Some("projects"));
        let key = self.key();

        let projects = services.cache().cached(partition, key, move || Box::pin(async move {
            client.get_projects().await.wrap_err_as_user(
                "Failed to fetch Todoist projects.",
                &[
                    "Check that your Todoist API token is valid and has the necessary permissions.",
                ],
            )
        }), chrono::Duration::hours(24)).await?;

        let project = projects.into_iter().find(|p| p.name == name).ok_or_else(|| {
            human_errors::user(
                format!("Todoist project '{}' not found.", name),
                &[
                    "Ensure that the specified project name is correct.",
                ],
            )
        })?;

        Ok(project.id)
    }

    async fn get_section_id(&self, project_name: &str, project_id: &str, name: Option<&str>, services: &impl crate::services::Services, client: Arc<TodoistWrapper>) -> Result<Option<String>, human_errors::Error> {
        if let Some(section_name) = name {
            let partition = self.partition(Some("sections"));
            let key = self.key();

            let sections = services.cache().cached(partition, key, move || Box::pin(async move {
                client.get_sections().await.wrap_err_as_user(
                    "Failed to fetch Todoist sections.",
                    &[
                        "Check that your Todoist API token is valid and has the necessary permissions.",
                    ],
                )
            }), chrono::Duration::hours(24)).await?;

            let section = sections.into_iter().find(|s| s.project_id == project_id && s.name == *section_name).ok_or_else(|| {
                human_errors::user(
                    format!("Todoist section '{}' not found in project '{}'.", section_name, project_name),
                    &[
                        "Ensure that the specified section name is correct.",
                    ],
                )
            })?;

            Ok(Some(section.id))
        } else {
            Ok(None)
        }
    }
}