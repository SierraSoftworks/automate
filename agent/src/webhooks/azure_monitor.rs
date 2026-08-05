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

/// What one person asked us to do with their Azure Monitor alerts.
///
/// There is deliberately no shared secret here. Each workflow is reached at its
/// own unguessable URL which its owner can rotate, so a secret would be a second
/// thing to configure that says exactly what the address already says.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct AzureMonitorWebhookConfig {
    /// What to call this workflow, so that somebody watching two different
    /// action groups can tell which of them filed a task.
    pub name: String,

    #[serde(default)]
    pub filter: Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: TodoistTarget,
}

impl std::fmt::Display for AzureMonitorWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "azure-monitor/{}", self.name)
    }
}

fn default_todoist_config() -> crate::publishers::TodoistTarget {
    crate::publishers::TodoistTarget {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

pub struct AzureMonitorWebhook;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Files a Todoist task when an Azure Monitor alert fires, and completes it when
the alert resolves. Both are keyed on the alert's id, so a flapping alert
reuses one task rather than leaving a trail of them. The task links back to the
alert in the Azure portal and its priority follows the alert's severity, with
Sev0 raised most urgently.

## Getting the address

Save the workflow first. Its address is generated when it is created and shown
on the workflow afterwards; there is nothing to paste into Azure until then.

Then, in the Azure portal, open **Monitor → Alerts → Action groups** and
either create a group or edit an existing one:

1. Under **Actions**, add an action of type **Webhook**.
2. Set the URI to this workflow's address.
3. Turn **Enable the common alert schema** on.
4. Attach the action group to the alert rules whose alerts you want here.

Step 3 is not optional. Without the common alert schema, Azure sends a
different payload shape for every monitor service — metric alerts, log alerts
and Service Health each have their own — and none of them will parse. If
nothing at all is arriving, that toggle is the first thing to check.

## Choosing which alerts to file

The filter runs against each alert and can match on `alert_id`, `alert_rule`,
`severity`, `monitor_condition`, `monitor_service` and `alert_target_ids`.

`severity` is the number from Azure's `Sev0` to `Sev4`, so a *smaller* value is
more urgent and a `<=` comparison is what you want:

```
severity <= 1
```

`alert_target_ids` is a list of the affected resource ids, so membership works
for scoping a workflow to one resource:

```
"/subscriptions/.../resourceGroups/prod/providers/..." in alert_target_ids
```

Leave the filter empty to file every alert the action group sends. Note that
the filter is only consulted when an alert fires — a resolution always
completes its task, so tightening the filter later cannot strand a task that is
already open.
"#;

crate::register_job!(AzureMonitorWebhook);
crate::register_workflow_type!(AzureMonitorWebhook);

impl crate::workflows::ConfigurableWorkflow for AzureMonitorWebhook {
    type ConfigType = AzureMonitorWebhookConfig;

    fn type_id() -> &'static str {
        "azure-monitor"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Azure Monitor".to_string(),
            description:
                "Files a task when an Azure Monitor alert fires, and completes it when the alert resolves."
                    .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Webhook {
                // Must name the same partition this job consumes from, or a
                // delivery would be queued somewhere nothing is reading.
                source: "azure-monitor".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(AzureMonitorWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Production alerts".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(AzureMonitorWebhookConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec![
                            "alert_id".into(),
                            "alert_rule".into(),
                            "severity".into(),
                            "monitor_condition".into(),
                            "monitor_service".into(),
                            "alert_target_ids".into(),
                        ],
                    },
                )
                .with_help(
                    "Only file alerts matching this, such as severity <= 1. Severity is the number from Sev0 to Sev4, so a smaller one is more urgent. Leave it empty to file every alert.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                AzureMonitorWebhookConfig,
                project = Some("Life"),
                section = Some("Tasks & Chores")
            ))
            .collect(),
        }
    }
}

impl Job for AzureMonitorWebhook {
    type JobType = WebhookDelivery;

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

        // Read now rather than carried in the payload, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<AzureMonitorWebhookConfig>(services).await? else {
            return Ok(());
        };

        let event: AzureMonitorAlertEventPayload = job.event.json()?;

        match event.data.essentials.monitor_condition {
            CommonAlertSchemaMonitorCondition::Fired if config.filter.matches(&event)? => {
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
                        config: config.todoist.clone(),
                        ..Default::default()
                    }, None, services).await?;

                Ok(())
            }
            CommonAlertSchemaMonitorCondition::Resolved => {
                TodoistCompleteTask::dispatch(
                    #[allow(clippy::needless_update)]
                    TodoistCompleteTaskPayload {
                        unique_key: event.data.essentials.alert_id,
                        config: config.todoist.clone(),
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
    use std::collections::HashMap;

    use super::*;
    use crate::webhooks::WebhookEvent;
    use crate::workflow_store::{WorkflowDraft, WorkflowStore};

    const RESOLVED_PAYLOAD: &str = r#"{"schemaId":"azureMonitorCommonAlertSchema","data":{"essentials":{"alertId":"/subscriptions/00000000-0000-0000-0000-000000000000/providers/Microsoft.AlertsManagement/alerts/11111111-1111-1111-1111-111111111111","alertRule":"vm availability - example-vm","targetResourceType":"microsoft.compute/virtualmachines","alertRuleID":"/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/example-rg/providers/microsoft.insights/metricAlerts/vm availability - example-vm","severity":"Sev3","signalType":"Metric","monitorCondition":"Resolved","targetResourceGroup":"example-rg","monitoringService":"Platform","alertTargetIDs":["/subscriptions/00000000-0000-0000-0000-000000000000/resourcegroups/example-rg/providers/microsoft.compute/virtualmachines/example-vm"],"configurationItems":["example-vm"],"originAlertId":"00000000-0000-0000-0000-000000000000_example-rg_microsoft.insights_metricAlerts_vm availability - example-vm_81493074","firedDateTime":"2026-06-12T01:13:01.9491785Z","resolvedDateTime":"2026-06-12T01:13:01.9491785Z","description":"","essentialsVersion":"1.0","alertContextVersion":"1.0","investigationLink":"https://portal.azure.com/"},"alertContext":{"properties":null,"conditionType":"MultipleResourceMultipleMetricCriteria","condition":{"windowSize":"PT5M","allOf":[{"metricName":"VmAvailabilityMetric","metricNamespace":"Microsoft.Compute/virtualMachines","operator":"LessThan","threshold":"1","timeAggregation":"Average","dimensions":[],"metricValue":1.0,"webTestName":null}],"staticThresholdFailingPeriods":{"numberOfEvaluationPeriods":0,"minFailingPeriodsToAlert":0},"windowStartTime":"2026-06-12T01:05:49.957Z","windowEndTime":"2026-06-12T01:10:49.957Z"}},"customProperties":null}}"#;

    /// The same alert while it is still firing, so the two differ only in the
    /// one field the handler branches on.
    fn firing_payload() -> String {
        RESOLVED_PAYLOAD.replace(
            r#""monitorCondition":"Resolved""#,
            r#""monitorCondition":"Fired""#,
        )
    }

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "azure-monitor".into(),
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
        AzureMonitorWebhook
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
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

    #[test]
    fn a_common_alert_schema_delivery_is_read_as_azure_sends_it() {
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
    async fn a_resolved_alert_completes_the_task_it_filed() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, serde_json::json!({ "name": "Platform" })).await;

        run(&services, &delivery(workflow, RESOLVED_PAYLOAD))
            .await
            .expect("Webhook should handle resolved alert");

        assert_eq!(
            queued(&services, "todoist/complete-task").await.len(),
            1,
            "an alert that has stopped firing should close the task it opened",
        );
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
                "name": "Platform",
                "filter": "monitor_service == \"ServiceHealth\"",
            }),
        )
        .await;

        run(&services, &delivery(workflow, firing_payload()))
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
        let workflow = store(&services, serde_json::json!({ "name": "Platform" })).await;

        WorkflowStore::new(&services)
            .with_index(&services)
            .delete(workflow)
            .await
            .unwrap();

        run(&services, &delivery(workflow, RESOLVED_PAYLOAD))
            .await
            .expect("a deleted workflow should not fail the delivery");

        assert!(queued(&services, "todoist/complete-task").await.is_empty());
    }

    #[test]
    fn deliveries_are_queued_where_this_workflow_reads_them() {
        // The trigger decides where a configuration is stored and the job
        // decides where deliveries are queued. A mismatch between the two is a
        // workflow that saves happily and never runs.
        use crate::workflows::ConfigurableWorkflow;

        assert_eq!(
            AzureMonitorWebhook::descriptor().trigger.partition(),
            <AzureMonitorWebhook as Job>::partition(),
        );
    }
}
