use serde::Deserialize;

use crate::prelude::*;

#[derive(Clone, Deserialize, Default)]
pub struct GrafanaWebhookConfig {
    #[serde(default)]
    pub secret: Option<String>,

    #[serde(default = "default_todoist_config")]
    pub todoist: crate::config::TodoistConfig,
}

fn default_todoist_config() -> crate::config::TodoistConfig {
    crate::config::TodoistConfig {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

pub struct GrafanaWebhook;

impl Job for GrafanaWebhook {
    type JobType = super::WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/grafana"
    }

    #[instrument("webhooks.grafana.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        Ok(())
    }
}