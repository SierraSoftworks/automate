use chrono::DateTime;
use serde::Deserialize;

use crate::{prelude::*, publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate}};

#[derive(Clone, Deserialize, Default)]
pub struct TailscaleWebhookConfig {
    pub secret: String,

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

#[derive(Clone)]
pub struct TailscaleWebhook;

impl Job for TailscaleWebhook {
    type JobType = super::WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/tailscale"
    }

    async fn handle(&self, job: &Self::JobType, services: impl Services + Send + Sync + 'static) -> Result<(), human_errors::Error> {
        // TODO: Validate the Tailscale webhook signature header
        // https://tailscale.com/kb/1213/webhooks#verifying-an-event-signature

        let event: TailscaleAlertEventPayload = job.json()?;

        let pretty_payload = serde_json::to_string_pretty(&event.data) 
            .unwrap_or_else(|_| job.body.clone());
        
        TodoistCreateTask::dispatch(
            TodoistCreateTaskPayload {
                title: format!(
                    "[**Tailscale**](https://login.tailscale.com/admin): {}",
                    event.message
                ),
                description: Some(format!("```\n{pretty_payload}\n```")),
                due: TodoistDueDate::DateTime(event.timestamp),
                priority: Some(match event._type.as_str() {
                    "exitNodeIPForwardingNotEnabled" => 4,
                    "subnetIPForwardingNotEnabled" => 4,
                    "nodeNeedsApproval" => 4,
                    "nodeKeyExpired" => 4,
                    "userNeedsApproval" => 4,

                    "policyUpdate" => 3,
                    "nodeCreated" => 3,
                    "nodeApproved" => 3,
                    "nodeKeyExpiringInOneDay" => 3,
                    "userCreated" => 3,
                    "userApproved" => 3,
                    "userRoleUpdated" => 3,

                    "nodeDeleted" => 2,
                    "webhookUpdated" => 2,
                    "webhookDeleted" => 2,

                    "test" => 1,

                    _ => 3
                }),
                config: services.config().webhooks.tailscale.todoist.clone(),
                ..Default::default()
            },
            None,
            &services,
        )
        .await?;

        Ok(())
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct TailscaleAlertEventPayload {
    version: u32,
    timestamp: DateTime<chrono::Utc>,
    #[serde(rename = "type")]
    _type: String,
    tailnet: String,
    message: String,
    data: serde_json::Value,
}