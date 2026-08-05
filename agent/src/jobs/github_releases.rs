use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::prelude::*;
use crate::publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate};
use crate::{collectors::GitHubReleasesCollector, filter::Filter, publishers::TodoistTarget};

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubReleasesConfig {
    pub repository: String,

    #[serde(default)]
    pub filter: Filter,

    #[serde(default)]
    pub todoist: TodoistTarget,
}

impl Display for GitHubReleasesConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "github-releases/{}", self.repository)
    }
}

#[derive(Clone)]
pub struct GitHubReleasesWorkflow;

crate::register_job!(GitHubReleasesWorkflow);
crate::register_workflow_type!(GitHubReleasesWorkflow);

impl crate::workflows::ConfigurableWorkflow for GitHubReleasesWorkflow {
    type ConfigType = GitHubReleasesConfig;

    fn type_id() -> &'static str {
        "github-releases"
    }

    fn describe(config: &Self::JobType) -> String {
        config.repository.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: <Self as crate::workflows::ConfigurableWorkflow>::type_id().to_string(),
            name: "GitHub Releases".to_string(),
            description: "Files a task for each new release of a repository.".to_string(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "@daily".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(GitHubReleasesConfig: repository),
                    "Repository",
                    FieldKind::Text {
                        placeholder: Some("SierraSoftworks/automate".into()),
                    },
                )
                .with_help("The repository to watch, as owner/name.")
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(GitHubReleasesConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec![
                            "tag".into(),
                            "name".into(),
                            "published".into(),
                            "link".into(),
                            "draft".into(),
                            "prerelease".into(),
                        ],
                    },
                )
                .with_help("Only file releases matching this, such as `prerelease == false`."),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                GitHubReleasesConfig,
                project = Some("Software"),
                section = Some("Updates")
            ))
            .collect(),
        }
    }
}

impl Job for GitHubReleasesWorkflow {
    type JobType = GitHubReleasesConfig;

    fn partition() -> &'static str {
        "github/releases/todoist"
    }

    /// Visibility timeout / retry backoff. Calls the rate-limited GitHub API, so
    /// a failed run waits an hour before retrying.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::hours(1)
    }

    #[instrument("workflow.github_releases.setup", skip(self, services), err(Display))]
    async fn setup(
        &self,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let config = services.config();
        CronJob::schedule(&config.workflows.github_releases, services).await
    }

    #[instrument("workflow.github_releases.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let collector = GitHubReleasesCollector::new(&job.repository);

        let items = collector.list(services).await?;

        for item in items.into_iter() {
            match job.filter.matches(&item) {
                Ok(false) => continue,
                Err(err) => {
                    return Err(err);
                }
                _ => {}
            }

            TodoistCreateTask::dispatch(
                TodoistCreateTaskPayload {
                    title: format!(
                        "[github:{}]({}): Released {} ({})",
                        &job.repository, &item.html_url, item.name, item.tag_name
                    ),
                    description: item.body.map(|body| {
                        crate::parsers::html_to_markdown(
                            &body,
                            "https://github.com/".parse().unwrap(),
                        )
                    }),
                    due: TodoistDueDate::Today,
                    config: job.todoist.clone(),
                    ..Default::default()
                },
                None,
                services,
            )
            .await?;
        }

        Ok(())
    }
}
