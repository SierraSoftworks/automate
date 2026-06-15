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

crate::register_job!(AzureMonitorWebhook);

impl Job for AzureMonitorWebhook {
    type JobType = super::WebhookEvent;

    fn partition() -> &'static str {
        "webhooks/azure-monitor"
    }

    #[instrument("webhooks.azure_monitor.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

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
                    }, None, services).await?;

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
                    services,
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
    fn get(&self, key: &str) -> FilterValue<'_> {
        match key {
            "alert_id" => self.data.essentials.alert_id.as_str().into(),
            "alert_rule" => self.data.essentials.alert_rule.as_str().into(),
            "severity" => (&self.data.essentials.severity).into(),
            "monitor_condition" => (&self.data.essentials.monitor_condition).into(),
            "monitor_service" => self.data.essentials.monitor_service.as_str().into(),
            "alert_target_ids" => self
                .data
                .essentials
                .alert_target_ids
                .iter()
                .map(|s| s.as_str().into())
                .collect::<Vec<_>>()
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
    #[serde(rename = "alertContextVersion")]
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
    #[serde(rename = "monitoringService")]
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

impl<'a> From<&CommonAlertSchemaSeverity> for FilterValue<'a> {
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

impl<'a> From<&CommonAlertSchemaMonitorCondition> for FilterValue<'a> {
    fn from(value: &CommonAlertSchemaMonitorCondition) -> Self {
        match value {
            CommonAlertSchemaMonitorCondition::Fired => "fired".into(),
            CommonAlertSchemaMonitorCondition::Resolved => "resolved".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhooks::WebhookEvent;
    use std::collections::HashMap;

    const RESOLVED_PAYLOAD: &str = r#"{"schemaId":"azureMonitorCommonAlertSchema","data":{"essentials":{"alertId":"/subscriptions/00000000-0000-0000-0000-000000000000/providers/Microsoft.AlertsManagement/alerts/11111111-1111-1111-1111-111111111111","alertRule":"vm availability - example-vm","targetResourceType":"microsoft.compute/virtualmachines","alertRuleID":"/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/example-rg/providers/microsoft.insights/metricAlerts/vm availability - example-vm","severity":"Sev3","signalType":"Metric","monitorCondition":"Resolved","targetResourceGroup":"example-rg","monitoringService":"Platform","alertTargetIDs":["/subscriptions/00000000-0000-0000-0000-000000000000/resourcegroups/example-rg/providers/microsoft.compute/virtualmachines/example-vm"],"configurationItems":["example-vm"],"originAlertId":"00000000-0000-0000-0000-000000000000_example-rg_microsoft.insights_metricAlerts_vm availability - example-vm_81493074","firedDateTime":"2026-06-12T01:13:01.9491785Z","resolvedDateTime":"2026-06-12T01:13:01.9491785Z","description":"","essentialsVersion":"1.0","alertContextVersion":"1.0","investigationLink":"https://portal.azure.com/"},"alertContext":{"properties":null,"conditionType":"MultipleResourceMultipleMetricCriteria","condition":{"windowSize":"PT5M","allOf":[{"metricName":"VmAvailabilityMetric","metricNamespace":"Microsoft.Compute/virtualMachines","operator":"LessThan","threshold":"1","timeAggregation":"Average","dimensions":[],"metricValue":1.0,"webTestName":null}],"staticThresholdFailingPeriods":{"numberOfEvaluationPeriods":0,"minFailingPeriodsToAlert":0},"windowStartTime":"2026-06-12T01:05:49.957Z","windowEndTime":"2026-06-12T01:10:49.957Z"}},"customProperties":null}}"#;

    #[test]
    fn test_deserialize_common_alert_schema() {
        let event: AzureMonitorAlertEventPayload =
            serde_json::from_str(RESOLVED_PAYLOAD).expect("payload should deserialize");

        assert_eq!(event.schema_id, "azureMonitorCommonAlertSchema");
        assert_eq!(
            event.data.essentials.alert_rule,
            "vm availability - example-vm"
        );
        assert_eq!(event.data.essentials.alert_context_version, "1.0");
        assert!(matches!(
            event.data.essentials.monitor_condition,
            CommonAlertSchemaMonitorCondition::Resolved
        ));
    }

    #[tokio::test]
    async fn test_azure_monitor_webhook_resolved() {
        let services = crate::testing::mock_services().await.unwrap();
        let webhook = AzureMonitorWebhook;

        let event = WebhookEvent {
            body: RESOLVED_PAYLOAD.to_string(),
            query: String::new(),
            headers: HashMap::new(),
        };

        let result = webhook
            .handle(
                JobContext::new(services, chrono::Utc::now(), None, None),
                &event,
            )
            .await;
        assert!(result.is_ok(), "Webhook should handle resolved alert");
    }
}
