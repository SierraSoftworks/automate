use std::{borrow::Cow, sync::Arc};

use serde::{Deserialize, Serialize};
use todoist_api::TodoistWrapper;

use automate_api::ConnectionId;

use crate::connections::{ConnectionSecret, ConnectionStore};
use crate::prelude::*;

/// The provider name under which Todoist accounts are linked.
pub const TODOIST_PROVIDER: &str = "todoist";

/// Where a workflow files the tasks it creates.
///
/// Carried by each workflow rather than inherited from a global default. An
/// installation used to have one Todoist account, so a global key with per-job
/// overrides made sense; once several people each have their own, "the Todoist
/// account" is not a thing that exists, and a workflow has to say which one it
/// means.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TodoistTarget {
    /// Which linked Todoist account to publish to.
    ///
    /// Optional, because the common case is one linked account and making
    /// somebody name it would be noise. When it is left out and there is exactly
    /// one, that one is used; when there are several, the ambiguity is reported
    /// rather than guessed at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<ConnectionId>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

impl TodoistTarget {
    /// A target naming a project and section, for the compiled-in defaults.
    #[allow(dead_code)]
    pub fn new(project: impl Into<String>, section: impl Into<String>) -> Self {
        Self {
            connection: None,
            project: Some(project.into()),
            section: Some(section.into()),
        }
    }
}

pub struct TodoistClient(pub Arc<TodoistWrapper>);

impl TodoistClient {
    /// Builds a client for the Todoist account a target names.
    pub async fn connect(
        services: &impl Services,
        target: &TodoistTarget,
    ) -> Result<Self, human_errors::Error> {
        let store = ConnectionStore::for_services(services);

        let connection = match target.connection {
            Some(id) => store.get(id).await?.ok_or_else(|| {
                human_errors::user(
                    format!("This workflow publishes to a Todoist account ('{id}') that is no longer connected."),
                    &[
                        "Reconnect the account, or point this workflow at a different one.",
                    ],
                )
            })?,
            None => {
                let mut linked = store.list_for_provider(TODOIST_PROVIDER).await?;

                match linked.len() {
                    1 => linked.remove(0),
                    0 => {
                        return Err(human_errors::user(
                            "No Todoist account is connected.",
                            &["Connect your Todoist account before running workflows that publish to it."],
                        ));
                    }
                    // Picking one would silently file somebody's tasks in the
                    // wrong place, which is worse than refusing to guess.
                    _ => {
                        return Err(human_errors::user(
                            "Several Todoist accounts are connected, so we cannot tell which this workflow should publish to.",
                            &["Choose the account this workflow should use."],
                        ));
                    }
                }
            }
        };

        let token = match store.open(&connection)? {
            ConnectionSecret::ApiKey { key } => key,
            ConnectionSecret::OAuth2 { access_token, .. } => access_token,
            other => {
                return Err(human_errors::system(
                    format!(
                        "The connection '{}' holds a {} credential, which cannot be used with Todoist.",
                        connection.id,
                        other.kind().as_str()
                    ),
                    &["Reconnect the account."],
                ));
            }
        };

        Ok(Self(Arc::new(TodoistWrapper::new(token))))
    }

    pub fn escape_content(content: &str) -> Cow<'_, str> {
        if !content.contains('@') && !content.contains('#') {
            Cow::Borrowed(content)
        } else if let Ok(re) = regex::Regex::new(r#"(@[^\s]+)"#) {
            let result = re.replace_all(content, r"`$1`");
            Cow::Owned(result.into_owned())
        } else {
            Cow::Borrowed(content)
        }
    }

    /// The projects in this account, as cached for name resolution.
    ///
    /// The same list the publisher resolves names against, so a project offered
    /// in the UI is one a workflow can actually file into.
    #[instrument("publishers.todoist.projects", skip(self, services), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    pub async fn projects(
        &self,
        services: &impl crate::services::Services,
    ) -> Result<Vec<todoist_api::models::Project>, human_errors::Error> {
        let client = self.0.clone();

        services
            .cache()
            .cached(
                "todoist/projects",
                "default",
                move || {
                    Box::pin(async move {
                        let mut projects = Vec::new();
                        let mut cursor = None;

                        loop {
                            let response = client.get_projects(None, cursor).await.wrap_user_err(
                                "Failed to fetch Todoist projects.",
                                &[
                                    "Check that your Todoist API token is valid and has the necessary permissions.",
                                ],
                            )?;

                            projects.extend(response.results);
                            cursor = response.next_cursor;

                            if cursor.is_none() {
                                break;
                            }
                        }

                        Ok(projects)
                    })
                },
                chrono::Duration::hours(24),
            )
            .await
    }

    /// The sections within a project.
    ///
    /// Todoist returns every section in one call rather than per project, so the
    /// whole set is cached once and narrowed here — which is also what the
    /// publisher does when resolving a section by name.
    #[instrument("publishers.todoist.sections", skip(self, services), fields(otel.kind=?OpenTelemetrySpanKind::Client), err(Display))]
    pub async fn sections(
        &self,
        project_id: &str,
        services: &impl crate::services::Services,
    ) -> Result<Vec<todoist_api::models::Section>, human_errors::Error> {
        let client = self.0.clone();

        let sections: Vec<todoist_api::models::Section> = services
            .cache()
            .cached(
                "todoist/sections",
                "default",
                move || {
                    Box::pin(async move {
                        let mut sections = Vec::new();
                        let mut cursor = None;

                        loop {
                            let response = client.get_sections(None, cursor).await.wrap_user_err(
                                "Failed to fetch Todoist sections.",
                                &["Check that your Todoist API token is valid."],
                            )?;

                            sections.extend(response.results);
                            cursor = response.next_cursor;

                            if cursor.is_none() {
                                break;
                            }
                        }

                        Ok(sections)
                    })
                },
                chrono::Duration::hours(24),
            )
            .await?;

        Ok(sections
            .into_iter()
            .filter(|section| section.project_id == project_id)
            .collect())
    }

    #[instrument("publishers.todoist.get_project_id", skip(self, name, services), fields(project.name = name), err(Display))]
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
                      let mut projects = Vec::new();
                      let mut cursor = None;

                      loop {
                        let response = client.get_projects(None, cursor).await.wrap_user_err(
                            "Failed to fetch Todoist projects.",
                            &[
                                "Check that your Todoist API token is valid and has the necessary permissions.",
                            ],
                        )?;

                        projects.extend(response.results);
                        cursor = response.next_cursor;

                        if cursor.is_none() {
                            break;
                        }
                      }

                      Ok(projects)
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

    #[instrument("publishers.todoist.get_section_id", skip(self, project_name, project_id, name, services), fields(project.name = project_name, section.name = ?name), err(Display))]
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
                          let mut sections = Vec::new();
                          let mut cursor = None;
                          loop {
                            let response = client.get_sections(None, cursor).await.wrap_user_err(
                                "Failed to fetch Todoist sections.",
                                &[
                                    "Check that your Todoist API token is valid and has the necessary permissions.",
                                ],
                            )?;
                            sections.extend(response.results);
                            cursor = response.next_cursor;
                            if cursor.is_none() {
                                break;
                            }
                          }

                          Ok(sections)
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

/// The form fields that let somebody say where a workflow files its tasks.
///
/// Every workflow that publishes to Todoist asks the same three questions, so
/// they are declared once. The type is a parameter rather than a fixed one
/// because [`crate::config_path!`] checks each path against the configuration
/// that actually holds it — a workflow whose target lives somewhere else, or is
/// not called `todoist`, will not compile against this.
///
/// The project and section defaults differ per workflow, since where a comic
/// belongs is not where a calendar event does.
#[macro_export]
macro_rules! todoist_target_fields {
    ($ty:ty, project = $project:expr, section = $section:expr) => {{
        let connection = automate_api::FieldDescriptor::new(
            $crate::config_path!($ty: todoist.connection),
            "Todoist account",
            automate_api::FieldKind::Connection {
                provider: $crate::publishers::TODOIST_PROVIDER.to_string(),
                connection_kind: Some(automate_api::ConnectionKind::ApiKey),
            },
        )
        .with_help("Which linked account the tasks are created in.")
        .required();

        let mut project = automate_api::FieldDescriptor::new(
            $crate::config_path!($ty: todoist.project),
            "Project",
            automate_api::FieldKind::Options {
                source: "projects".into(),
                depends_on: $crate::config_path!($ty: todoist.connection).into(),
                parent: None,
            },
        );
        if let Some(default) = $project {
            project = project.with_default(default);
        }

        let mut section = automate_api::FieldDescriptor::new(
            $crate::config_path!($ty: todoist.section),
            "Section",
            automate_api::FieldKind::Options {
                source: "sections".into(),
                depends_on: $crate::config_path!($ty: todoist.connection).into(),
                // Sections belong to a project, so offering every section in the
                // workspace would let somebody file into one that is not in the
                // project they chose.
                parent: Some($crate::config_path!($ty: todoist.project).to_string()),
            },
        );
        if let Some(default) = $section {
            section = section.with_default(default);
        }

        [connection, project, section]
    }};
}

#[cfg(test)]
mod target_tests {
    use super::*;
    use crate::services::AppContext;

    fn alice() -> TenantId {
        TenantId::new("alice").unwrap()
    }

    async fn link(context: &AppContext, name: &str) -> ConnectionId {
        ConnectionStore::new(context.tenant(alice()), alice())
            .create(
                TODOIST_PROVIDER,
                name,
                None,
                ConnectionSecret::ApiKey {
                    key: format!("tok-{name}"),
                },
            )
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn a_single_linked_account_does_not_have_to_be_named() {
        // The common case is one account, and making somebody name it would be
        // noise in every workflow they create.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        link(&context, "personal").await;

        assert!(
            TodoistClient::connect(&context.tenant(alice()), &TodoistTarget::default())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn an_ambiguous_target_is_reported_rather_than_guessed_at() {
        // Picking one would file somebody's tasks in the wrong account, which is
        // worse than refusing.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        link(&context, "personal").await;
        link(&context, "work").await;

        let Err(err) =
            TodoistClient::connect(&context.tenant(alice()), &TodoistTarget::default()).await
        else {
            panic!("an ambiguous target should be refused");
        };
        assert!(
            err.to_string().contains("Several Todoist accounts"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn naming_an_account_resolves_it_even_when_several_are_linked() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        link(&context, "personal").await;
        let work = link(&context, "work").await;

        let target = TodoistTarget {
            connection: Some(work),
            ..Default::default()
        };

        assert!(
            TodoistClient::connect(&context.tenant(alice()), &target)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn publishing_with_no_account_connected_explains_what_to_do() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();

        let Err(err) =
            TodoistClient::connect(&context.tenant(alice()), &TodoistTarget::default()).await
        else {
            panic!("publishing without an account should be refused");
        };
        assert!(err.to_string().contains("No Todoist account"), "{err}");
    }

    #[tokio::test]
    async fn a_target_naming_an_unlinked_account_is_reported() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        link(&context, "personal").await;

        let target = TodoistTarget {
            connection: Some(ConnectionId::from_entropy(999)),
            ..Default::default()
        };

        assert!(
            TodoistClient::connect(&context.tenant(alice()), &target)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn one_account_cannot_publish_through_anothers_connection() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let hers = link(&context, "personal").await;
        let bob = TenantId::new("bob").unwrap();

        let target = TodoistTarget {
            connection: Some(hers),
            ..Default::default()
        };

        assert!(
            TodoistClient::connect(&context.tenant(bob), &target)
                .await
                .is_err(),
            "a connection must not be usable from another account"
        );
    }
}
