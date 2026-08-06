use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::prelude::*;
use crate::publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate};
use crate::{
    collectors::{GitHubReleasesCollector, IncrementalCollector},
    db::StateKey,
    filter::Filter,
    publishers::TodoistTarget,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubReleasesConfig {
    pub repository: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<automate_api::ConnectionId>,

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

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Watches one repository's releases and files a Todoist task for each new one,
carrying the release notes into the task's body so you can decide whether an
upgrade is worth your afternoon without opening GitHub.

The first run files every release on the repository's first page of results —
up to thirty of them — rather than only what appears afterwards. Every run
after that only sees releases published since the newest one it has seen, so
the burst happens once.

## Naming the repository

**Repository** is `owner/name` as it appears in the repository's own address —
`SierraSoftworks/automate` for `https://github.com/SierraSoftworks/automate`.
Not the full URL, and not a display name.

The selected GitHub personal access token authenticates every poll, avoiding the
much smaller anonymous API rate limit. It also determines which private
repositories can be read; GitHub reports an inaccessible private repository as
not found rather than admitting that it exists.

This is a poll, not a subscription: no webhook is set up on the repository and
nothing is written to it, so you do not need any access beyond reading.

## Choosing which releases to file

The filter runs against each release and can match on `tag`, `name`,
`published`, `link`, `draft` and `prerelease`. The two worth knowing about are
the booleans — plenty of projects cut release candidates far more often than
releases, and a task per candidate is how a useful workflow becomes noise.

```
prerelease == false && draft == false
```

Matching on the tag is the other common case, for repositories that release
several components from one place:

```
tag startswith "cli/"
```

## Scheduling

This polls the GitHub API, which is rate limited, so a failed run backs off for
an hour before it tries again. `@daily` suits most repositories.
"#;

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

    fn state(config: &Self::ConfigType) -> Vec<StateKey> {
        vec![GitHubReleasesCollector::new(&config.repository).state()]
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{
            ConnectionKind, FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor,
        };

        WorkflowTypeDescriptor {
            id: <Self as crate::workflows::ConfigurableWorkflow>::type_id().to_string(),
            name: "GitHub Releases".to_string(),
            description: "Files a task for each new release of a repository.".to_string(),
            documentation: DOCUMENTATION.to_string(),
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
                    crate::config_path!(GitHubReleasesConfig: connection),
                    "GitHub account",
                    FieldKind::Connection {
                        provider: crate::integrations::github_app::GITHUB_PROVIDER.to_string(),
                        connection_kind: Some(ConnectionKind::ApiKey),
                    },
                )
                .with_help("Which GitHub personal access token is used to poll for releases.")
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

    #[instrument("workflow.github_releases.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let connection = job.connection.ok_or_else(|| {
            human_errors::user(
                "This GitHub Releases workflow has no GitHub account selected.",
                &["Edit the workflow and select a GitHub personal access token connection."],
            )
        })?;
        let api_key = crate::connections::resolve_api_key(
            connection,
            crate::integrations::github_app::GITHUB_PROVIDER,
            services,
        )
        .await?;
        let collector = GitHubReleasesCollector::with_api_key(&job.repository, api_key);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::ConfigurableWorkflow;

    #[test]
    fn releases_require_a_github_pat_connection() {
        let descriptor = GitHubReleasesWorkflow::descriptor();
        let connection = descriptor
            .fields
            .iter()
            .find(|field| field.name == "connection")
            .unwrap();

        assert!(connection.required);
        assert!(matches!(
            &connection.kind,
            automate_api::FieldKind::Connection {
                provider,
                connection_kind: Some(automate_api::ConnectionKind::ApiKey),
            } if provider == crate::integrations::github_app::GITHUB_PROVIDER
        ));
    }
}
