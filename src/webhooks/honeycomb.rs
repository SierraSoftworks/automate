use serde::Deserialize;

use crate::{prelude::*, publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate}};

#[derive(Clone, Deserialize, Default)]
pub struct HoneycombWebhookConfig {
    pub trusted_secrets: Vec<String>,

    #[serde(default)]
    pub filter: crate::filter::Filter,

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

pub struct HoneycombWebhook;

impl Job for HoneycombWebhook {
    type JobType = super::WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/honeycomb"
    }

    #[instrument("webhooks.honeycomb.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(&self, job: &Self::JobType, services: impl Services + Send + Sync + 'static) -> Result<(), human_errors::Error> {
        if let Some(secret) = job.headers.get("X-Honeycomb-Webhook-Token") {
            if !services.config().webhooks.honeycomb.trusted_secrets.contains(secret) {
                warn!("Received Honeycomb webhook with untrusted secret '{}'; rejecting request.", secret);
                return Ok(());
            }
        } else if services.config().webhooks.honeycomb.trusted_secrets.is_empty() {
            debug!("No Honeycomb webhook secret configured; skipping verification.");
        } else {
            warn!("Received Honeycomb webhook without secret, but secrets are configured; rejecting request.");
            return Ok(());
        }
        
        let event: HoneycombAlertEventPayload = job.json()?;

        if !event.status.eq_ignore_ascii_case("triggered") {
            info!("Ignoring non-triggered Honeycomb alert: {}", event.status);
            return Ok(());
        }

        if !services.config().webhooks.honeycomb.filter.matches(&event)? {
            info!("Honeycomb alert '{}' did not match filter; ignoring.", event.name);
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
                config: services.config().webhooks.honeycomb.todoist.clone(),
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

impl Filterable for HoneycombAlertEventPayload {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "id" => self.id.clone().into(),
            "name" => self.name.clone().into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}