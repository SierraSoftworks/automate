use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    collectors::{GitHubNotificationsCollector, IncrementalCollector},
    db::StateKey,
    prelude::*,
};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct GitHubNotificationsCleanupConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<automate_api::ConnectionId>,

    #[serde(default)]
    pub filter: Filter,
}

impl Display for GitHubNotificationsCleanupConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "github-notifications-cleanup")
    }
}

#[derive(Clone)]
pub struct GitHubNotificationsCleanupWorkflow;

crate::register_job!(GitHubNotificationsCleanupWorkflow);
crate::register_workflow_type!(GitHubNotificationsCleanupWorkflow);

impl crate::workflows::ConfigurableWorkflow for GitHubNotificationsCleanupWorkflow {
    type ConfigType = GitHubNotificationsCleanupConfig;

    fn type_id() -> &'static str {
        "github-notifications-cleanup"
    }

    fn describe(_config: &Self::ConfigType) -> String {
        "GitHub notifications cleanup".to_string()
    }

    /// The `If-Modified-Since` watermark, which is kept against the GitHub API
    /// rather than this workflow, so clearing it makes every notification look
    /// new to anything reading the same inbox.
    fn state(_config: &Self::ConfigType) -> Vec<StateKey> {
        vec![GitHubNotificationsCollector::new().state()]
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{
            ConnectionKind, FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor,
        };

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "GitHub Notifications Cleanup".to_string(),
            description: "Marks notifications done once their issue or pull request is closed."
                .to_string(),
            documentation: r#"## What this does

Checks your GitHub notifications and marks threads done when their issue or pull
request has been closed or merged. It uses the selected personal access token,
so each user cleans up only their own notification inbox.

The token needs the `notifications` scope and access to any private repositories
whose notification subjects should be checked.
"#
            .to_string(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "@hourly".to_string(),
            },
            fields: vec![
                FieldDescriptor::new(
                    crate::config_path!(GitHubNotificationsCleanupConfig: connection),
                    "GitHub account",
                    FieldKind::Connection {
                        provider: crate::integrations::github_app::GITHUB_PROVIDER.to_string(),
                        connection_kind: Some(ConnectionKind::ApiKey),
                    },
                )
                .with_help("Which GitHub personal access token owns the notifications to clean up.")
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(GitHubNotificationsCleanupConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec![
                            "reason".into(),
                            "repository.name".into(),
                            "repository.full_name".into(),
                            "repository.owner".into(),
                            "subject.title".into(),
                            "subject.type".into(),
                            "unread".into(),
                        ],
                    },
                )
                .with_help("Only clean up notifications matching this expression."),
            ],
        }
    }
}

impl Job for GitHubNotificationsCleanupWorkflow {
    type JobType = GitHubNotificationsCleanupConfig;

    fn partition() -> &'static str {
        "github/notifications/cleanup"
    }

    /// Visibility timeout / retry backoff. Reconciles GitHub notifications
    /// against Todoist, hitting both rate-limited APIs, so a failed run waits an
    /// hour before retrying.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::hours(1)
    }

    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let connection = job.connection.ok_or_else(|| {
            human_errors::user(
                "This GitHub notifications cleanup workflow has no GitHub account selected.",
                &["Edit the workflow and select a GitHub personal access token connection."],
            )
        })?;
        let api_key = crate::connections::resolve_api_key(
            connection,
            crate::integrations::github_app::GITHUB_PROVIDER,
            services,
        )
        .await?;
        let collector = GitHubNotificationsCollector::with_api_key(api_key);

        let (notifications, _) = collector.fetch_since(None, services).await?;

        for notification in notifications {
            if !job.filter.matches(&notification)? {
                continue;
            }

            if let Some(subject) = collector
                .get_subject(&notification.subject, services)
                .await?
                && !subject.is_open()
            {
                collector.mark_as_done(&notification.id, services).await?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::ConfigurableWorkflow;

    #[test]
    fn cleanup_is_a_scheduled_workflow_requiring_a_github_pat() {
        let descriptor = GitHubNotificationsCleanupWorkflow::descriptor();
        assert!(matches!(
            descriptor.trigger,
            automate_api::WorkflowTrigger::Cron { .. }
        ));

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
