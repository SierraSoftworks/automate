use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    collectors::{
        GitHubNotificationsCollector, GitHubNotificationsSubjectState, IncrementalCollector,
    },
    prelude::*,
};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct GitHubNotificationsCleanupConfig {
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

impl Job for GitHubNotificationsCleanupWorkflow {
    type JobType = GitHubNotificationsCleanupConfig;

    fn partition() -> &'static str {
        "workflow/github-notifications-cleanup"
    }

    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let collector = GitHubNotificationsCollector::new();

        let (notifications, _) = collector.fetch_since(None, &services).await?;

        for notification in notifications {
            if !job.filter.matches(&notification)? {
                continue;
            }

            if let Some(subject) = collector
                .get_subject(&notification.subject, &services)
                .await?
                && subject.state != GitHubNotificationsSubjectState::Open
            {
                collector.mark_as_done(&notification.id, &services).await?;
            }
        }

        Ok(())
    }
}
