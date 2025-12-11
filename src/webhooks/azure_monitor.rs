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
pub struct AzureMonitorWebhookConfig {
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

pub struct AzureMonitorWebhook;

impl Job for AzureMonitorWebhook {
    type JobType = super::WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/azure-monitor"
    }

    #[instrument("webhooks.azure_monitor.handle", skip(self, job, services), fields(job = %job))]
    async fn handle(
        &self,
        job: &Self::JobType,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let event: AzureMonitorAlertEventPayload = job.json()?;

        match event.data.essentials.monitor_condition {
            CommonAlertSchemaMonitorCondition::Fired
                if services
                    .config()
                    .webhooks
                    .azure_monitor
                    .filter
                    .matches(&event)? =>
            {
                TodoistUpsertTask::dispatch(
                    TodoistUpsertTaskPayload {
                        unique_key: event.data.essentials.alert_id.clone(),
                        title: format!(
                            "[{}](https://portal.azure.com/#blade/Microsoft_Azure_Monitoring_Alerts/AlertDetails.ReactView/alertId/{}): {}",
                            event.data.essentials.monitor_service,
                            urlencoding::encode(&event.data.essentials.alert_id),
                            event.data.essentials.alert_rule
                        ),
                        description: event.data.essentials.description.clone(),
                        due: crate::publishers::TodoistDueDate::DateTime(event.data.essentials.fired_date_time),
                        priority: Some(event.data.essentials.severity.priority()),
                        config: services.config().webhooks.azure_monitor.todoist.clone(),
                        ..Default::default()
                    }, None, &services).await?;

                Ok(())
            }
            CommonAlertSchemaMonitorCondition::Resolved => {
                TodoistCompleteTask::dispatch(
                    #[allow(clippy::needless_update)]
                    TodoistCompleteTaskPayload {
                        unique_key: event.data.essentials.alert_id,
                        config: services.config().webhooks.azure_monitor.todoist.clone(),
                        ..Default::default()
                    },
                    None,
                    &services,
                )
                .await?;
                Ok(())
            }
            _ => {
                info!(
                    "Ignoring non-matching Azure Monitor alert: {}",
                    event.data.essentials.alert_rule
                );
                Ok(())
            }
        }
    }
}

pub type AzureMonitorAlertEventPayload = CommonAlertSchema;

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct CommonAlertSchema {
    #[serde(rename = "schemaId")]
    pub schema_id: String,
    pub data: CommonAlertSchemaData,
}

impl Filterable for CommonAlertSchema {
    fn get(&self, key: &str) -> crate::filter::FilterValue {
        match key {
            "alert_id" => self.data.essentials.alert_id.clone().into(),
            "alert_rule" => self.data.essentials.alert_rule.clone().into(),
            "severity" => (&self.data.essentials.severity).into(),
            "monitor_condition" => (&self.data.essentials.monitor_condition).into(),
            "monitor_service" => self.data.essentials.monitor_service.clone().into(),
            "alert_target_ids" => self
                .data
                .essentials
                .alert_target_ids
                .iter()
                .map(|s| s.clone().into())
                .collect::<Vec<FilterValue>>()
                .into(),
            _ => FilterValue::Null,
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct CommonAlertSchemaData {
    pub essentials: CommonAlertSchemaEssentials,
    #[serde(rename = "alertContext")]
    pub alert_context: serde_json::Value,
    #[serde(default, rename = "customProperties")]
    pub custom_properties: Option<serde_json::Value>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct CommonAlertSchemaEssentials {
    #[serde(rename = "essentialsVersion")]
    pub essentials_version: String,
    #[serde(rename = "alertContentVersion")]
    pub alert_context_version: String,

    #[serde(rename = "alertId")]
    pub alert_id: String,
    #[serde(rename = "alertRule")]
    pub alert_rule: String,
    pub severity: CommonAlertSchemaSeverity,
    #[serde(rename = "signalType")]
    pub signal_type: String,
    #[serde(rename = "monitorCondition")]
    pub monitor_condition: CommonAlertSchemaMonitorCondition,
    #[serde(rename = "monitorService")]
    pub monitor_service: String,
    #[serde(rename = "alertTargetIDs")]
    pub alert_target_ids: Vec<String>,
    #[serde(rename = "configurationItems")]
    pub configuration_items: Vec<String>,
    #[serde(rename = "originAlertId")]
    pub origin_alert_id: String,
    #[serde(rename = "firedDateTime")]
    pub fired_date_time: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "resolvedDateTime")]
    pub resolved_date_time: Option<chrono::DateTime<chrono::Utc>>,
    pub description: Option<String>,
}

#[derive(Deserialize)]
pub enum CommonAlertSchemaSeverity {
    Sev0,
    Sev1,
    Sev2,
    Sev3,
    Sev4,
}

impl CommonAlertSchemaSeverity {
    pub fn priority(&self) -> i32 {
        match self {
            CommonAlertSchemaSeverity::Sev0 => 4,
            CommonAlertSchemaSeverity::Sev1 => 3,
            CommonAlertSchemaSeverity::Sev2 => 2,
            CommonAlertSchemaSeverity::Sev3 => 1,
            CommonAlertSchemaSeverity::Sev4 => 1,
        }
    }
}

impl From<&CommonAlertSchemaSeverity> for FilterValue {
    fn from(value: &CommonAlertSchemaSeverity) -> Self {
        match value {
            CommonAlertSchemaSeverity::Sev0 => 0.into(),
            CommonAlertSchemaSeverity::Sev1 => 1.into(),
            CommonAlertSchemaSeverity::Sev2 => 2.into(),
            CommonAlertSchemaSeverity::Sev3 => 3.into(),
            CommonAlertSchemaSeverity::Sev4 => 4.into(),
        }
    }
}

#[derive(Deserialize)]
pub enum CommonAlertSchemaMonitorCondition {
    Fired,
    Resolved,
}

impl From<&CommonAlertSchemaMonitorCondition> for FilterValue {
    fn from(value: &CommonAlertSchemaMonitorCondition) -> Self {
        match value {
            CommonAlertSchemaMonitorCondition::Fired => "fired".into(),
            CommonAlertSchemaMonitorCondition::Resolved => "resolved".into(),
        }
    }
}
