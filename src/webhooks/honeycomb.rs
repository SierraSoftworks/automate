use serde::Deserialize;

use crate::{prelude::*, publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate}};

fn default_todoist_config() -> crate::config::TodoistConfig {
    crate::config::TodoistConfig {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

pub struct HoneycombWebhook;

impl Job for HoneycombWebhook {
    type JobType = super::WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/honeycomb"
    }

    async fn handle(&self, job: &Self::JobType, services: impl Services + Send + Sync + 'static) -> Result<(), human_errors::Error> {
        // TODO: Validate the Honeycomb webhook signature header (X-Honeycomb-Webhook-Token) matches expected value
        
        let event: HoneycombAlertEventPayload = job.json()?;

        if !event.status.eq_ignore_ascii_case("triggered") {
            info!("Ignoring non-triggered Honeycomb alert: {}", event.status);
            return Ok(());
        }
        
        TodoistCreateTask::dispatch(
            TodoistCreateTaskPayload {
                title: format!(
                    "[**Honeycomb Alert**]({}): {}",
                    event.result_url.or(event.trigger_url).unwrap_or_else(|| "https://ui.honeycomb.io".into()),
                    event.name
                ),
                description: event.description,
                due: TodoistDueDate::DateTime(chrono::Utc::now()),
                priority: Some(4),
                config: default_todoist_config(),
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
struct HoneycombAlertEventPayload {
    version: String,
    shared_secret: Option<String>,

    name: String,
    id: String,
    trigger_description: Option<String>,

    status: String, // TRIGGERED | OK
    summary: String,
    description: Option<String>,
    operator: String,
    threshold: f64,

    result_url: Option<String>,
    trigger_url: Option<String>,
}