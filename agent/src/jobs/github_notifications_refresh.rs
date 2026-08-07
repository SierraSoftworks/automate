use std::fmt::Display;

use chrono::TimeDelta;

use crate::jobs::{GitHubNotificationsConfig, GitHubNotificationsWorkflow};
use crate::prelude::*;
use crate::workflows::ConfigurableWorkflow;

/// How long a refresh waits before running, so that a burst of webhook
/// deliveries collapses into a single fetch of the notifications API.
const DEBOUNCE: TimeDelta = TimeDelta::seconds(30);

/// The idempotency key every scheduled refresh shares, so at most one is ever
/// pending.
const REFRESH_KEY: &str = "refresh";

#[derive(Clone, Serialize, Deserialize)]
pub struct GitHubNotificationsRefreshTask {
    /// The webhook event which prompted the refresh, recorded for tracing.
    pub trigger: String,
}

impl Display for GitHubNotificationsRefreshTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "github-notifications-refresh({})", self.trigger)
    }
}

/// Fans a webhook-triggered refresh out to every stored
/// [`GitHubNotificationsWorkflow`], so that notifications are collected when
/// something actually happens rather than on a fixed poll interval.
#[derive(Clone)]
pub struct GitHubNotificationsRefreshWorkflow;

impl GitHubNotificationsRefreshWorkflow {
    /// Schedules a debounced refresh, unless one is already pending.
    ///
    /// Re-enqueueing under [`REFRESH_KEY`] would slide the delay forward on
    /// every delivery, so an organisation which is continuously active could
    /// starve the fetch indefinitely. Skipping instead bounds the work at one
    /// fetch per [`DEBOUNCE`] window while guaranteeing it happens.
    #[instrument(
        "workflow.github_notifications_refresh.schedule",
        skip(services),
        err(Display)
    )]
    pub async fn schedule(
        trigger: &str,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<(), human_errors::Error> {
        let pending = services
            .queue()
            .peek::<_, serde_json::Value>(Self::partition(), 1)
            .await?;

        if !pending.is_empty() {
            debug!(
                "A GitHub notifications refresh is already scheduled; ignoring the '{trigger}' event."
            );
            return Ok(());
        }

        Self::dispatch_delayed(
            GitHubNotificationsRefreshTask {
                trigger: trigger.to_string(),
            },
            Some(REFRESH_KEY.into()),
            DEBOUNCE,
            services,
        )
        .await
    }
}

crate::register_job!(GitHubNotificationsRefreshWorkflow);

impl Job for GitHubNotificationsRefreshWorkflow {
    type JobType = GitHubNotificationsRefreshTask;

    fn partition() -> &'static str {
        "github/notifications/refresh"
    }

    #[instrument("workflow.github_notifications_refresh.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let workflows = crate::workflow_store::WorkflowStore::new(services)
            .records()
            .await?;
        let workflows: Vec<_> = workflows
            .into_iter()
            .filter(|workflow| {
                workflow.enabled && workflow.type_id == GitHubNotificationsWorkflow::type_id()
            })
            .collect();

        if workflows.is_empty() {
            debug!("No GitHub notification workflows are stored; ignoring {job}.");
            return Ok(());
        }

        for workflow in workflows {
            let config: GitHubNotificationsConfig = serde_json::from_value(workflow.config)
                .map_err(|err| {
                    human_errors::system(
                        format!("A stored GitHub notifications workflow could not be read: {err}"),
                        &["Edit and save the workflow again."],
                    )
                })?;

            // Dispatched without an idempotency key, matching the cron path, so
            // that a refresh can never overwrite a collection which is already
            // in flight.
            GitHubNotificationsWorkflow::dispatch(config, None, services).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_store::{WorkflowDraft, WorkflowStore};

    async fn pending<S: Services>(services: &S, partition: &'static str) -> usize {
        services
            .queue()
            .peek::<_, serde_json::Value>(partition, 10)
            .await
            .expect("peek the queue")
            .len()
    }

    #[tokio::test]
    async fn bursts_of_deliveries_schedule_a_single_refresh() {
        let services = crate::testing::mock_services()
            .await
            .expect("build mock services");

        for trigger in ["pull_request", "issue_comment", "issues"] {
            GitHubNotificationsRefreshWorkflow::schedule(trigger, &services)
                .await
                .expect("the refresh should be scheduled");
        }

        let scheduled = services
            .queue()
            .peek::<_, GitHubNotificationsRefreshTask>(
                GitHubNotificationsRefreshWorkflow::partition(),
                10,
            )
            .await
            .expect("peek the refresh queue");

        assert_eq!(scheduled.len(), 1, "a burst should collapse into one fetch");
        assert_eq!(
            scheduled[0].payload.trigger, "pull_request",
            "the first delivery should win, so its delay is never slid forward"
        );
        assert!(scheduled[0].hidden_until > chrono::Utc::now());
    }

    #[tokio::test]
    async fn a_refresh_fans_out_to_every_stored_workflow() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .expect("build mock services");
        let store = WorkflowStore::new(&services);

        for connection in [1, 2] {
            store
                .create(WorkflowDraft {
                    type_id: GitHubNotificationsWorkflow::type_id().to_string(),
                    config: serde_json::json!({
                        "connection": automate_api::ConnectionId::from_entropy(connection),
                    }),
                    schedule: Some("@hourly".to_string()),
                    enabled: true,
                })
                .await
                .expect("store a notifications workflow");
        }

        GitHubNotificationsRefreshWorkflow
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &GitHubNotificationsRefreshTask {
                    trigger: "pull_request".to_string(),
                },
            )
            .await
            .expect("the refresh should fan out");

        assert_eq!(
            pending(&services, GitHubNotificationsWorkflow::partition()).await,
            2
        );
    }

    #[tokio::test]
    async fn a_refresh_without_stored_workflows_does_nothing() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .expect("build mock services");

        GitHubNotificationsRefreshWorkflow
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                &GitHubNotificationsRefreshTask {
                    trigger: "pull_request".to_string(),
                },
            )
            .await
            .expect("the refresh should be a no-op");

        assert_eq!(
            pending(&services, GitHubNotificationsWorkflow::partition()).await,
            0
        );
    }
}
