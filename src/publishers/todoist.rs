use std::{borrow::Cow, sync::Arc};

use serde::{Deserialize, Serialize};
use todoist_api::TodoistWrapper;

use crate::prelude::*;

pub struct TodoistClient(pub Arc<TodoistWrapper>);

impl TodoistClient {
    pub fn new(config: &crate::config::TodoistConfig) -> Result<Self, human_errors::Error> {
        let api_token = config.api_key.clone().ok_or_else(|| {
            human_errors::user("You have not provided a Todoist API key.", &[
                "Set the 'connection.todoist.api_key' secret to a valid Todoist API token to enable publishing.",
            ])
        })?;

        Ok(Self(Arc::new(TodoistWrapper::new(api_token))))
    }

    pub fn escape_content(content: &str) -> Cow<'_, str> {
        if !content.contains('@') && !content.contains('#') {
            Cow::Borrowed(content)
        } else {
            if let Ok(re) = regex::Regex::new(r#"(@[^\s]+)"#) {
                let result = re.replace_all(content, r"`$1`");
                Cow::Owned(result.into_owned())
            } else {
                Cow::Borrowed(content)
            }
        }
    }

    pub async fn get_project_id(
        &self,
        name: &str,
        services: &impl crate::services::Services,
    ) -> Result<String, human_errors::Error> {
        let partition = "todoist/projects";
        let key = "default";

        let client = self.0.clone();

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

    pub async fn get_section_id(
        &self,
        project_name: &str,
        project_id: &str,
        name: Option<&str>,
        services: &impl crate::services::Services,
    ) -> Result<Option<String>, human_errors::Error> {
        if let Some(section_name) = name {
            let partition = "todoist/sections";
            let key = "default";

            let client = self.0.clone();

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
