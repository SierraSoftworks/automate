use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::{
    config::TodoistConfig,
    filter::FilterValue,
    prelude::*,
    publishers::{
        TodoistCompleteTask, TodoistCompleteTaskPayload, TodoistUpsertTask,
        TodoistUpsertTaskPayload,
    },
};

#[derive(Clone, Deserialize, Default)]
pub struct GrafanaWebhookConfig {
    /// Optional authorization header value for webhook authentication
    #[serde(default)]
    pub secret: Option<String>,

    /// Filter to apply to incoming alerts
    #[serde(default)]
    pub filter: Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: TodoistConfig,
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
        // Validate the authorization header if a secret is configured
        if let Some(expected_secret) = &services.config().webhooks.grafana.secret {
            if !expected_secret.is_empty() {
                // HTTP headers are case-insensitive, so we need to search for the header with case-insensitive comparison
                let auth_header = job
                    .headers
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                    .map(|(_, value)| value.as_str());

                if let Some(auth_header) = auth_header {
                    if auth_header != expected_secret {
                        warn!(
                            "Received Grafana webhook with invalid authorization header; rejecting request."
                        );
                        return Ok(());
                    }
                } else {
                    warn!(
                        "Received Grafana webhook without authorization header, but secret is configured; rejecting request."
                    );
                    return Ok(());
                }
            } else {
                debug!(
                    "No Grafana webhook secret configured; skipping authorization verification."
                );
            }
        }

        let event: GrafanaAlertPayload = job.json()?;

        // Apply filter to the entire alert payload
        if !services.config().webhooks.grafana.filter.matches(&event)? {
            info!(
                "Grafana alert '{}' did not match filter; ignoring.",
                event.title
            );
            return Ok(());
        }

        // Process based on alert status
        match event.status.as_str() {
            "firing" => {
                // Create a unique key based on the alert rule URL or title
                let unique_key = event
                    .rule_url
                    .clone()
                    .unwrap_or_else(|| format!("grafana-alert-{}", event.title));

                // Get the first alert for more details
                let first_alert = event.alerts.first();
                let starts_at = first_alert.and_then(|a| a.starts_at);
                let severity = first_alert
                    .and_then(|a| a.labels.get("severity"))
                    .map(|s| s.as_str())
                    .unwrap_or("unknown");

                // Determine priority based on severity label
                let priority = match severity {
                    "critical" => 4,
                    "error" => 3,
                    "warning" => 2,
                    _ => 1,
                };

                // Create or update the Todoist task
                TodoistUpsertTask::dispatch(
                    TodoistUpsertTaskPayload {
                        unique_key: unique_key.clone(),
                        title: format!(
                            "[**Grafana Alert**]({}): {}",
                            event
                                .rule_url
                                .as_ref()
                                .or(event.external_url.as_ref())
                                .unwrap_or(&"https://grafana.com".to_string()),
                            event.title
                        ),
                        description: Some(event.message.clone()),
                        due: starts_at
                            .map(crate::publishers::TodoistDueDate::DateTime)
                            .unwrap_or_else(|| {
                                crate::publishers::TodoistDueDate::DateTime(Utc::now())
                            }),
                        priority: Some(priority),
                        config: services.config().webhooks.grafana.todoist.clone(),
                        ..Default::default()
                    },
                    None,
                    &services,
                )
                .await?;

                Ok(())
            }
            "resolved" => {
                // Complete the task when the alert is resolved
                let unique_key = event
                    .rule_url
                    .clone()
                    .unwrap_or_else(|| format!("grafana-alert-{}", event.title));

                TodoistCompleteTask::dispatch(
                    TodoistCompleteTaskPayload {
                        unique_key,
                        config: services.config().webhooks.grafana.todoist.clone(),
                    },
                    None,
                    &services,
                )
                .await?;

                Ok(())
            }
            _ => {
                info!(
                    "Ignoring Grafana alert with status '{}': {}",
                    event.status, event.title
                );
                Ok(())
            }
        }
    }
}

/// Grafana alert webhook payload structure
#[allow(dead_code)]
#[derive(Deserialize)]
pub struct GrafanaAlertPayload {
    /// Name of the contact point (receiver)
    pub receiver: String,
    /// Overall status: "firing" or "resolved"
    pub status: String,
    /// Organization ID
    #[serde(rename = "orgId")]
    pub org_id: i64,
    /// Alert title
    pub title: String,
    /// Alert state: "alerting", "ok", etc.
    pub state: String,
    /// Alert message
    pub message: String,
    /// Grafana base URL
    #[serde(rename = "externalURL")]
    pub external_url: Option<String>,
    /// Direct URL to the alerting rule
    #[serde(rename = "ruleUrl")]
    pub rule_url: Option<String>,
    /// List of individual alerts
    pub alerts: Vec<GrafanaAlert>,
}

impl Filterable for GrafanaAlertPayload {
    fn get(&self, key: &str) -> FilterValue {
        match key {
            "receiver" => self.receiver.clone().into(),
            "status" => self.status.clone().into(),
            "org_id" => self.org_id.into(),
            "title" => self.title.clone().into(),
            "state" => self.state.clone().into(),
            "message" => self.message.clone().into(),
            "alerts" => self
                .alerts
                .iter()
                .map(|a| a.status.clone().into())
                .collect::<Vec<FilterValue>>()
                .into(),
            _ => FilterValue::Null,
        }
    }
}

/// Individual alert within a Grafana alert notification
#[allow(dead_code)]
#[derive(Deserialize)]
pub struct GrafanaAlert {
    /// Alert status: "firing" or "resolved"
    pub status: String,
    /// Labels associated with the alert
    pub labels: std::collections::HashMap<String, String>,
    /// Annotations associated with the alert
    pub annotations: std::collections::HashMap<String, String>,
    /// When the alert started firing
    #[serde(rename = "startsAt")]
    pub starts_at: Option<DateTime<Utc>>,
    /// When the alert stopped firing (null if still firing)
    #[serde(rename = "endsAt")]
    pub ends_at: Option<DateTime<Utc>>,
    /// URL to the panel/dashboard that generated the alert
    #[serde(rename = "generatorURL")]
    pub generator_url: Option<String>,
    /// Alert values (metrics that triggered the alert)
    #[serde(default)]
    pub values: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhooks::WebhookEvent;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_grafana_webhook_firing() {
        let services = crate::testing::mock_services().await.unwrap();
        let webhook = GrafanaWebhook;

        let body = r#"{
            "receiver": "my-webhook",
            "status": "firing",
            "orgId": 1,
            "title": "[FIRING:1] High CPU usage",
            "state": "alerting",
            "message": "CPU usage is above 90%",
            "externalURL": "http://localhost:3000",
            "ruleUrl": "http://localhost:3000/alerting/rule/1",
            "alerts": [
                {
                    "status": "firing",
                    "labels": {
                        "severity": "critical",
                        "instance": "localhost:9090"
                    },
                    "annotations": {
                        "summary": "High CPU usage detected"
                    },
                    "startsAt": "2025-12-11T22:05:00Z",
                    "endsAt": null,
                    "generatorURL": "http://localhost:3000/d/xyz?viewPanel=2",
                    "values": {
                        "cpu_usage": 95
                    }
                }
            ]
        }"#;

        let event = WebhookEvent {
            body: body.to_string(),
            query: String::new(),
            headers: HashMap::new(),
        };

        let result = webhook.handle(&event, services).await;
        assert!(result.is_ok(), "Webhook should handle firing alert");
    }

    #[tokio::test]
    async fn test_grafana_webhook_resolved() {
        let services = crate::testing::mock_services().await.unwrap();
        let webhook = GrafanaWebhook;

        let body = r#"{
            "receiver": "my-webhook",
            "status": "resolved",
            "orgId": 1,
            "title": "[RESOLVED] High CPU usage",
            "state": "ok",
            "message": "CPU usage has returned to normal",
            "externalURL": "http://localhost:3000",
            "ruleUrl": "http://localhost:3000/alerting/rule/1",
            "alerts": [
                {
                    "status": "resolved",
                    "labels": {
                        "severity": "critical",
                        "instance": "localhost:9090"
                    },
                    "annotations": {
                        "summary": "High CPU usage detected"
                    },
                    "startsAt": "2025-12-11T22:05:00Z",
                    "endsAt": "2025-12-11T22:15:00Z",
                    "generatorURL": "http://localhost:3000/d/xyz?viewPanel=2",
                    "values": {
                        "cpu_usage": 60
                    }
                }
            ]
        }"#;

        let event = WebhookEvent {
            body: body.to_string(),
            query: String::new(),
            headers: HashMap::new(),
        };

        let result = webhook.handle(&event, services).await;
        assert!(result.is_ok(), "Webhook should handle resolved alert");
    }

    #[tokio::test]
    async fn test_grafana_webhook_invalid_json() {
        let services = crate::testing::mock_services().await.unwrap();
        let webhook = GrafanaWebhook;

        let body = r#"{"invalid json"#;

        let event = WebhookEvent {
            body: body.to_string(),
            query: String::new(),
            headers: HashMap::new(),
        };

        let result = webhook.handle(&event, services).await;
        assert!(result.is_err(), "Webhook should reject invalid JSON");
    }

    #[tokio::test]
    async fn test_grafana_alert_filterable() {
        let alert = GrafanaAlertPayload {
            receiver: "test-receiver".to_string(),
            status: "firing".to_string(),
            org_id: 123,
            title: "Test Alert".to_string(),
            state: "alerting".to_string(),
            message: "Test message".to_string(),
            external_url: Some("https://grafana.example.com".to_string()),
            rule_url: Some("https://grafana.example.com/rule/1".to_string()),
            alerts: vec![],
        };

        // Test Filterable trait implementation
        assert_eq!(
            alert.get("receiver"),
            FilterValue::from("test-receiver".to_string())
        );
        assert_eq!(alert.get("status"), FilterValue::from("firing".to_string()));
        assert_eq!(alert.get("org_id"), FilterValue::from(123));
        assert_eq!(
            alert.get("title"),
            FilterValue::from("Test Alert".to_string())
        );
        assert_eq!(
            alert.get("state"),
            FilterValue::from("alerting".to_string())
        );
        assert_eq!(
            alert.get("message"),
            FilterValue::from("Test message".to_string())
        );
        assert_eq!(alert.get("unknown_field"), FilterValue::Null);
    }

    #[test]
    fn test_grafana_webhook_header_case_insensitive() {
        // Test that header lookup works with different case variations
        let headers_lowercase = {
            let mut h = HashMap::new();
            h.insert("authorization".to_string(), "my-token".to_string());
            h
        };

        let found = headers_lowercase
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.as_str());
        assert!(found.is_some(), "Should find lowercase header");
        assert_eq!(found.unwrap(), "my-token");

        // Test with uppercase
        let headers_uppercase = {
            let mut h = HashMap::new();
            h.insert("AUTHORIZATION".to_string(), "my-token".to_string());
            h
        };

        let found = headers_uppercase
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.as_str());
        assert!(found.is_some(), "Should find uppercase header");

        // Test with mixed case
        let headers_mixed = {
            let mut h = HashMap::new();
            h.insert("Authorization".to_string(), "my-token".to_string());
            h
        };

        let found = headers_mixed
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.as_str());
        assert!(found.is_some(), "Should find mixed case header");
    }
}
