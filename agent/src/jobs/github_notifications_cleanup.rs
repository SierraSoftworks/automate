use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    collectors::{
        GitHubNotificationsCollector, GitHubNotificationsSubject, GitHubSubjectInformation,
        IncrementalCollector,
    },
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

impl GitHubNotificationsCleanupWorkflow {
    /// Whether this notification is one we can prove is finished with.
    ///
    /// Two conditions, and both are about not dismissing something a person
    /// still needs to read. The subject has to be an issue or a pull request,
    /// because that is the only thing GitHub gives an open/closed state and
    /// this workflow only promises to act on those. And that state has to say
    /// closed or merged, rather than merely failing to say open: releases,
    /// commits and anything whose payload we model imperfectly all arrive with
    /// no state at all, and "we could not tell" is not "it is done".
    fn is_finished(
        subject: &GitHubNotificationsSubject,
        information: Option<&GitHubSubjectInformation>,
    ) -> bool {
        subject.issue_reference().is_some() && information.is_some_and(|i| i.is_resolved())
    }
}

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

            // Asked before the subject is fetched, so a repository full of
            // releases and pushes does not cost a request each just to be
            // ignored.
            if notification.subject.issue_reference().is_none() {
                continue;
            }

            let information = collector
                .get_subject(&notification.subject, services)
                .await?;

            if Self::is_finished(&notification.subject, information.as_ref()) {
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

    fn subject(type_: &str, url: Option<&str>) -> GitHubNotificationsSubject {
        GitHubNotificationsSubject {
            title: "Bump serde".into(),
            type_: type_.into(),
            url: url.map(str::to_string),
            latest_comment_url: None,
        }
    }

    fn information(body: serde_json::Value) -> GitHubSubjectInformation {
        serde_json::from_value(body).expect("the sample subject should deserialize")
    }

    const PULL_REQUEST: &str = "https://api.github.com/repos/example/repo/pulls/1";

    #[test]
    fn an_open_pull_request_is_left_alone() {
        assert!(!GitHubNotificationsCleanupWorkflow::is_finished(
            &subject("PullRequest", Some(PULL_REQUEST)),
            Some(&information(serde_json::json!({ "state": "open" }))),
        ));
    }

    #[test]
    fn a_closed_pull_request_is_finished() {
        assert!(GitHubNotificationsCleanupWorkflow::is_finished(
            &subject("PullRequest", Some(PULL_REQUEST)),
            Some(&information(serde_json::json!({ "state": "closed" }))),
        ));
    }

    /// The case this workflow used to get wrong. Every field of
    /// [`GitHubSubjectInformation`] is optional, so a response carrying no
    /// `state` deserializes happily and used to be read as "not open", which
    /// dismissed the notification for a pull request nobody had touched.
    #[test]
    fn a_pull_request_we_could_not_read_a_state_from_is_left_alone() {
        assert!(!GitHubNotificationsCleanupWorkflow::is_finished(
            &subject("PullRequest", Some(PULL_REQUEST)),
            Some(&information(serde_json::json!({ "body": "Bumps serde" }))),
        ));
    }

    #[test]
    fn subjects_which_are_not_issues_or_pull_requests_are_left_alone() {
        assert!(!GitHubNotificationsCleanupWorkflow::is_finished(
            &subject(
                "Release",
                Some("https://api.github.com/repos/example/repo/releases/1")
            ),
            Some(&information(serde_json::json!({ "tag_name": "v1.0.0" }))),
        ));

        assert!(!GitHubNotificationsCleanupWorkflow::is_finished(
            &subject("CheckSuite", None),
            None,
        ));
    }

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
