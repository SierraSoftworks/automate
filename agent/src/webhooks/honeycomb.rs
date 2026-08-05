use serde::{Deserialize, Serialize};

use crate::{
    prelude::*,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate},
    webhooks::WebhookDelivery,
};

/// What one person asked us to do with their Honeycomb triggers.
///
/// There is deliberately no list of trusted secrets here. Honeycomb offered a
/// shared token because the endpoint it posted to was the same for everybody, so
/// the token was the only thing saying which installation a delivery belonged
/// to; a workflow now has its own unguessable URL that its owner can rotate, and
/// that answers the same question without anything to copy between two systems.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct HoneycombWebhookConfig {
    /// What to call this workflow, so that somebody with triggers from two
    /// Honeycomb environments can tell which of them filed a task.
    pub name: String,

    #[serde(default)]
    pub filter: crate::filter::Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: crate::publishers::TodoistTarget,
}

impl std::fmt::Display for HoneycombWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "honeycomb/{}", self.name)
    }
}

fn default_todoist_config() -> crate::publishers::TodoistTarget {
    crate::publishers::TodoistTarget {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

pub struct HoneycombWebhook;

crate::register_job!(HoneycombWebhook);
crate::register_workflow_type!(HoneycombWebhook);

impl crate::workflows::ConfigurableWorkflow for HoneycombWebhook {
    type ConfigType = HoneycombWebhookConfig;

    fn type_id() -> &'static str {
        "honeycomb"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Honeycomb".to_string(),
            description: "Files a task when a Honeycomb trigger fires.".to_string(),
            trigger: WorkflowTrigger::Webhook {
                // Must name the same partition this job consumes from, or a
                // delivery would be queued somewhere nothing is reading.
                source: "honeycomb".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(HoneycombWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Production triggers".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(HoneycombWebhookConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec!["id".into(), "name".into()],
                    },
                )
                .with_help(
                    "Only file triggers matching this, such as name == \"Slow requests\". Leave it empty to file every trigger that fires.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                HoneycombWebhookConfig,
                project = Some("Life"),
                section = Some("Tasks & Chores")
            ))
            .collect(),
        }
    }
}

impl Job for HoneycombWebhook {
    type JobType = WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/honeycomb"
    }

    #[instrument("webhooks.honeycomb.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the payload, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<HoneycombWebhookConfig>(services).await? else {
            return Ok(());
        };

        let event: HoneycombAlertEventPayload = job.event.json()?;

        if !event.status.eq_ignore_ascii_case("triggered") {
            info!("Ignoring non-triggered Honeycomb alert: {}", event.status);
            return Ok(());
        }

        if !config.filter.matches(&event)? {
            info!(
                "Honeycomb alert '{}' did not match filter; ignoring.",
                event.name
            );
            return Ok(());
        }

        TodoistCreateTask::dispatch(
            TodoistCreateTaskPayload {
                title: format!(
                    "[**Honeycomb Alert**]({}): {}",
                    event
                        .result_url
                        .or(event.trigger_url)
                        .unwrap_or_else(|| "https://ui.honeycomb.io".into()),
                    event.name
                ),
                description: event.description,
                due: TodoistDueDate::DateTime(ctx.scheduled_at()),
                priority: Some(4),
                config: config.todoist.clone(),
                ..Default::default()
            },
            None,
            services,
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
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        match key {
            "id" => self.id.as_str().into(),
            "name" => self.name.as_str().into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::webhooks::WebhookEvent;
    use crate::workflow_store::{WorkflowDraft, WorkflowStore};

    /// A trigger that has just fired, as Honeycomb posts one.
    fn triggered_payload() -> String {
        serde_json::json!({
            "version": "v0.1.0",
            "name": "Slow requests",
            "id": "abc123",
            "status": "TRIGGERED",
            "summary": "p99 latency is above 500ms",
            "description": "The API is slower than it should be.",
            "operator": ">",
            "threshold": 500.0,
            "result_url": "https://ui.honeycomb.io/example/triggers/abc123",
        })
        .to_string()
    }

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "honeycomb".into(),
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
        HoneycombWebhook
            .handle(
                JobContext::new(services.clone(), chrono::Utc::now(), None, None),
                delivery,
            )
            .await
    }

    async fn filed(
        services: &(impl Services + Send + Sync + 'static),
    ) -> Vec<crate::db::PeekedMessage<serde_json::Value>> {
        services
            .queue()
            .peek("todoist/create-task", 10)
            .await
            .expect("peek the todoist queue")
    }

    #[tokio::test]
    async fn a_trigger_that_fired_files_a_task_linking_back_to_it() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, serde_json::json!({ "name": "Production" })).await;

        run(&services, &delivery(workflow, triggered_payload()))
            .await
            .expect("Webhook should handle a triggered alert");

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0].payload["title"],
            "[**Honeycomb Alert**](https://ui.honeycomb.io/example/triggers/abc123): Slow requests",
        );
    }

    #[tokio::test]
    async fn a_trigger_that_has_recovered_files_nothing() {
        // Honeycomb posts on the way back down as well as on the way up, and a
        // task for something that has already fixed itself is noise.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, serde_json::json!({ "name": "Production" })).await;

        let recovered = triggered_payload().replace(r#""TRIGGERED""#, r#""OK""#);
        run(&services, &delivery(workflow, recovered))
            .await
            .unwrap();

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_trigger_the_workflows_filter_rejects_files_nothing() {
        // The filter now belongs to the workflow rather than the installation,
        // so this is also what proves the handler reads the stored record.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Production",
                "filter": "name == \"Something else\"",
            }),
        )
        .await;

        run(&services, &delivery(workflow, triggered_payload()))
            .await
            .unwrap();

        assert!(
            filed(&services).await.is_empty(),
            "a trigger the owner asked to ignore should not have filed anything",
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

        run(&services, &delivery(workflow, triggered_payload()))
            .await
            .expect("a deleted workflow should not fail the delivery");

        assert!(filed(&services).await.is_empty());
    }

    #[test]
    fn deliveries_are_queued_where_this_workflow_reads_them() {
        // The trigger decides where a configuration is stored and the job
        // decides where deliveries are queued. A mismatch between the two is a
        // workflow that saves happily and never runs.
        use crate::workflows::ConfigurableWorkflow;

        assert_eq!(
            HoneycombWebhook::descriptor().trigger.partition(),
            <HoneycombWebhook as Job>::partition(),
        );
    }
}
