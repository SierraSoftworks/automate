use std::fmt::Display;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    filter::FilterValue,
    prelude::*,
    publishers::TodoistTarget,
    publishers::{
        TodoistCompleteTask, TodoistCompleteTaskPayload, TodoistUpsertTask,
        TodoistUpsertTaskPayload,
    },
    webhooks::WebhookDelivery,
};

/// What one person asked us to do with their Grafana alerts.
///
/// There is deliberately no shared secret here. Grafana's contact point offered
/// a bearer token because the endpoint it posted to was the same for everybody;
/// a workflow now has its own unguessable URL that its owner can rotate, so a
/// token would be a second credential saying the same thing as the first — and
/// two ways to authorise the same delivery is one more than anybody needs to get
/// right.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct GrafanaWebhookConfig {
    /// What to call this workflow, so that somebody with alerts from two
    /// Grafana instances can tell which of them filed a task.
    pub name: String,

    /// Filter to apply to incoming alerts
    #[serde(default)]
    pub filter: Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: TodoistTarget,
}

impl Display for GrafanaWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "grafana/{}", self.name)
    }
}

fn default_todoist_config() -> crate::publishers::TodoistTarget {
    crate::publishers::TodoistTarget {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

pub struct GrafanaWebhook;

crate::register_job!(GrafanaWebhook);
crate::register_workflow_type!(GrafanaWebhook);

impl crate::workflows::ConfigurableWorkflow for GrafanaWebhook {
    type ConfigType = GrafanaWebhookConfig;

    fn type_id() -> &'static str {
        "grafana"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Grafana".to_string(),
            description:
                "Files a task when a Grafana alert starts firing, and completes it when the alert resolves."
                    .to_string(),
            trigger: WorkflowTrigger::Webhook {
                // Must name the same partition this job consumes from, or a
                // delivery would be queued somewhere nothing is reading.
                source: "grafana".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(GrafanaWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Production dashboards".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(GrafanaWebhookConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec![
                            "receiver".into(),
                            "status".into(),
                            "org_id".into(),
                            "title".into(),
                            "state".into(),
                            "message".into(),
                            "alerts.status".into(),
                        ],
                    },
                )
                .with_help(
                    "Only file alerts matching this, such as state == \"alerting\". Leave it empty to file every alert.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                GrafanaWebhookConfig,
                project = Some("Life"),
                section = Some("Tasks & Chores")
            ))
            .collect(),
        }
    }
}

impl Job for GrafanaWebhook {
    type JobType = WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/grafana"
    }

    #[instrument("webhooks.grafana.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the payload, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<GrafanaWebhookConfig>(services).await? else {
            return Ok(());
        };

        let event: GrafanaAlertPayload = job.event.json()?;

        // Apply filter to the entire alert payload
        if !config.filter.matches(&event)? {
            info!(
                "Grafana alert '{}' did not match filter; ignoring.",
                event.title
            );
            return Ok(());
        }

        // Process based on alert status
        match event.status {
            GrafanaAlertStatus::Firing => {
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

                let alert_title = event
                    .group_labels
                    .and_then(|l| {
                        l.get("grafana_folder")
                            .or_else(|| l.get("alertname"))
                            .cloned()
                    })
                    .or_else(|| {
                        event
                            .alerts
                            .iter()
                            .filter_map(|a| a.labels.get("rulename"))
                            .next()
                            .cloned()
                    })
                    .unwrap_or("Grafana Alert".to_string());
                let dashboard_url = event
                    .alerts
                    .iter()
                    .filter_map(|a| a.dashboard_url.clone())
                    .next()
                    .or_else(|| event.external_url.clone())
                    .unwrap_or_else(|| "https://grafana.com".into());

                let summary = event
                    .alerts
                    .iter()
                    .filter_map(|a| a.annotations.get("summary").cloned())
                    .collect::<Vec<String>>()
                    .join("\n");

                // Create or update the Todoist task
                TodoistUpsertTask::dispatch(
                    TodoistUpsertTaskPayload {
                        unique_key: unique_key.clone(),
                        title: format!(
                            "[**Grafana Alert**]({dashboard_url}): {alert_title} is unhealthy"
                        ),
                        description: Some(summary),
                        due: starts_at
                            .map(crate::publishers::TodoistDueDate::DateTime)
                            .unwrap_or_else(|| {
                                crate::publishers::TodoistDueDate::DateTime(ctx.scheduled_at())
                            }),
                        priority: Some(priority),
                        config: config.todoist.clone(),
                        ..Default::default()
                    },
                    Some(
                        event
                            .rule_url
                            .clone()
                            .unwrap_or_else(|| event.title.clone())
                            .into(),
                    ),
                    services,
                )
                .await?;

                Ok(())
            }
            GrafanaAlertStatus::Resolved => {
                // Complete the task when the alert is resolved
                let unique_key = event
                    .rule_url
                    .clone()
                    .unwrap_or_else(|| event.title.clone());

                TodoistCompleteTask::dispatch(
                    TodoistCompleteTaskPayload {
                        unique_key,
                        config: config.todoist.clone(),
                    },
                    None,
                    services,
                )
                .await?;

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
    pub status: GrafanaAlertStatus,
    /// Organization ID
    #[serde(rename = "orgId")]
    pub org_id: i64,
    /// Alert title
    pub title: String,
    /// Alert state: "alerting", "ok", etc.
    pub state: GrafanaAlertState,
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
    #[serde(rename = "groupLabels")]
    /// Common labels for the alert group
    pub group_labels: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "commonLabels")]
    pub common_labels: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "commonAnnotations")]
    pub common_annotations: Option<std::collections::HashMap<String, String>>,
}

impl Filterable for GrafanaAlertPayload {
    fn get(&self, key: &str) -> FilterValue<'_> {
        match key {
            "receiver" => self.receiver.as_str().into(),
            "status" => format!("{}", self.status).into(),
            "org_id" => self.org_id.into(),
            "title" => self.title.as_str().into(),
            "state" => format!("{}", self.state).into(),
            "message" => self.message.as_str().into(),
            "alerts.status" => self
                .alerts
                .iter()
                .map(|a| format!("{}", a.status).into())
                .collect::<Vec<_>>()
                .into(),
            _ => FilterValue::Null,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrafanaAlertStatus {
    Firing,
    Resolved,
}

impl Display for GrafanaAlertStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrafanaAlertStatus::Firing => write!(f, "firing"),
            GrafanaAlertStatus::Resolved => write!(f, "resolved"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrafanaAlertState {
    Alerting,
    Ok,
    NoData,
    Paused,
}

impl Display for GrafanaAlertState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrafanaAlertState::Alerting => write!(f, "alerting"),
            GrafanaAlertState::Ok => write!(f, "ok"),
            GrafanaAlertState::NoData => write!(f, "no_data"),
            GrafanaAlertState::Paused => write!(f, "paused"),
        }
    }
}

/// Individual alert within a Grafana alert notification
#[allow(dead_code)]
#[derive(Deserialize)]
pub struct GrafanaAlert {
    /// Alert status: "firing" or "resolved"
    pub status: GrafanaAlertStatus,
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
    #[serde(rename = "dashboardURL")]
    pub dashboard_url: Option<String>,
    /// Alert values (metrics that triggered the alert)
    #[serde(default)]
    pub values: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::webhooks::WebhookEvent;
    use crate::workflow_store::{WorkflowDraft, WorkflowStore};

    const FIRING_PAYLOAD: &str = r#"{
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

    const RESOLVED_PAYLOAD: &str = r#"{
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

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "grafana".into(),
                config,
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id
    }

    fn delivery(workflow: automate_api::WorkflowId, body: impl Into<String>) -> WebhookDelivery {
        WebhookDelivery {
            workflow,
            event: WebhookEvent {
                body: body.into(),
                query: String::new(),
                headers: HashMap::new(),
            },
        }
    }

    async fn run(
        services: &(impl Services + Send + Sync + Clone + 'static),
        delivery: &WebhookDelivery,
    ) -> Result<(), human_errors::Error> {
        GrafanaWebhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                delivery,
            )
            .await
    }

    async fn queued(
        services: &(impl Services + Send + Sync + 'static),
        partition: &'static str,
    ) -> Vec<crate::db::PeekedMessage<serde_json::Value>> {
        services
            .queue()
            .peek(partition, 10)
            .await
            .expect("peek the todoist queue")
    }

    #[tokio::test]
    async fn a_firing_alert_opens_a_task_for_it() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, serde_json::json!({ "name": "Production" })).await;

        run(&services, &delivery(workflow, FIRING_PAYLOAD))
            .await
            .expect("Webhook should handle firing alert");

        assert_eq!(queued(&services, "todoist/upsert-task").await.len(), 1);
    }

    #[tokio::test]
    async fn a_resolved_alert_completes_the_task_it_filed() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, serde_json::json!({ "name": "Production" })).await;

        run(&services, &delivery(workflow, RESOLVED_PAYLOAD))
            .await
            .expect("Webhook should handle resolved alert");

        assert_eq!(queued(&services, "todoist/complete-task").await.len(), 1);
    }

    #[tokio::test]
    async fn a_body_that_is_not_grafanas_is_refused() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, serde_json::json!({ "name": "Production" })).await;

        let result = run(&services, &delivery(workflow, r#"{"invalid json"#)).await;

        assert!(result.is_err(), "Webhook should reject invalid JSON");
    }

    #[tokio::test]
    async fn an_alert_the_workflows_filter_rejects_files_nothing() {
        // The filter now belongs to the workflow rather than the installation,
        // so this is also what proves the handler reads the stored record.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Production",
                "filter": "receiver == \"someone-elses-webhook\"",
            }),
        )
        .await;

        run(&services, &delivery(workflow, FIRING_PAYLOAD))
            .await
            .unwrap();

        assert!(
            queued(&services, "todoist/upsert-task").await.is_empty(),
            "an alert the owner asked to ignore should not have filed anything",
        );
    }

    #[tokio::test]
    async fn a_delivery_for_a_workflow_that_is_gone_stops_there() {
        // Deliveries queue behind one another, so a workflow can be deleted
        // while one of its own is still waiting to run.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, serde_json::json!({ "name": "Production" })).await;

        WorkflowStore::new(&services)
            .with_index(&services)
            .delete(workflow)
            .await
            .unwrap();

        run(&services, &delivery(workflow, FIRING_PAYLOAD))
            .await
            .expect("a deleted workflow should not fail the delivery");

        assert!(queued(&services, "todoist/upsert-task").await.is_empty());
    }

    #[test]
    fn an_alert_exposes_the_fields_the_filter_editor_offers() {
        let alert = GrafanaAlertPayload {
            receiver: "test-receiver".to_string(),
            status: GrafanaAlertStatus::Firing,
            org_id: 123,
            title: "Test Alert".to_string(),
            state: GrafanaAlertState::Alerting,
            message: "Test message".to_string(),
            external_url: Some("https://grafana.example.com".to_string()),
            rule_url: Some("https://grafana.example.com/rule/1".to_string()),
            alerts: vec![],
            group_labels: None,
            common_labels: None,
            common_annotations: None,
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
    fn deliveries_are_queued_where_this_workflow_reads_them() {
        // The trigger decides where a configuration is stored and the job
        // decides where deliveries are queued. A mismatch between the two is a
        // workflow that saves happily and never runs.
        use crate::workflows::ConfigurableWorkflow;

        assert_eq!(
            GrafanaWebhook::descriptor().trigger.partition(),
            <GrafanaWebhook as Job>::partition(),
        );
    }
}
