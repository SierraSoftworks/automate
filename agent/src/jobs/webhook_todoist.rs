//! A webhook workflow for services we have never heard of.
//!
//! Every other webhook in this agent is built around a payload we modelled:
//! `webhooks/sentry` knows what Sentry sends, deserializes it, and reads real
//! fields off a real struct. That is the right shape when we know the sender,
//! and it is no help at all when somebody wants to point their CI, their
//! doorbell, or an internal service nobody outside their company has heard of
//! at Automate. Writing a Rust struct is not a thing a user can do.
//!
//! So this workflow models nothing. The delivery is parsed as JSON and handed
//! straight to the user's own filter and templates, which address it by path —
//! see [`crate::webhook_payload`] for both halves of that. The cost is that we
//! can offer no field suggestions and no validation of what the sender actually
//! posts; the benefit is that the set of services Automate works with stops
//! being a list this crate maintains.
//!
//! # Why the queue payload is not the configuration
//!
//! [`crate::workflows::ConfigurableWorkflow`] is written around the shape a
//! cron workflow has, where the thing a run is handed *is* its configuration:
//! [`crate::jobs::CronJob`] reads the stored record and enqueues `record.config`
//! verbatim into the target partition. A webhook run needs two things that come
//! from different places — the delivery, which only the request has, and the
//! configuration, which only the record has — so the payload names the record
//! and carries the delivery, and the configuration is read at the moment it is
//! needed. That is also what keeps an edit from taking effect one delivery late.
//!
//! The trait nevertheless deserializes `Self::JobType` when it validates a
//! submitted form and when it names a stored workflow, so the payload type has
//! to accept a configuration too. [`WebhookTodoistTask`] therefore has a variant
//! for each shape rather than a bag of optional fields — validation that accepts
//! `{}` would be no validation at all, which is the one thing the workflow
//! registry is careful to avoid.

use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::{
    prelude::*,
    publishers::TodoistTarget,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate},
    webhook_payload::{JsonFilter, render},
};

/// What a person tells us about the deliveries they expect.
///
/// Deliberately small. Everything that varies between senders is expressed as a
/// path into the payload rather than as a field here, because the alternative is
/// this struct growing a field per service and being back where we started.
#[derive(Clone, Serialize, Deserialize)]
pub struct WebhookTodoistConfig {
    /// What to call this workflow, so it can be told apart from the others in a
    /// list of them.
    pub name: String,

    /// The task's title, rendered against the delivery's JSON body.
    pub title: String,

    /// The task's body, rendered the same way. Optional because plenty of
    /// notifications are entirely said by their title.
    #[serde(default)]
    pub description: Option<String>,

    /// Which deliveries are worth a task. Empty means all of them.
    #[serde(default)]
    pub filter: Filter,

    #[serde(default)]
    pub todoist: TodoistTarget,
}

impl Display for WebhookTodoistConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "webhook/{}", self.name)
    }
}

/// What arrives on this workflow's queue partition.
///
/// Only the delivery: it names the stored workflow whose configuration it should
/// be run against, and carries the request that arrived. The configuration is a
/// separate type because the two describe different things — what the sender
/// posted, and what the owner asked us to do with it.
#[derive(Clone, Serialize, Deserialize)]
pub struct WebhookTodoistTask {
    pub workflow: automate_api::WorkflowId,
    pub event: WebhookEvent,
}

impl Display for WebhookTodoistTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "webhook/{}", self.workflow)
    }
}

#[derive(Clone)]
pub struct WebhookTodoistWorkflow;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Gives you an address to post JSON at, and files a Todoist task for each
delivery that arrives. Unlike the other webhook types, this one knows nothing
about the sender: the body is parsed as JSON and handed straight to your filter
and your templates, which address it by path. That is what makes it the right
choice for a service Automate has never heard of — your CI, your doorbell, an
internal tool nobody outside your company has heard of.

Deliveries whose body is not JSON are discarded with a line in the log, because
there is nothing this workflow could do with them.

## Getting the address

Save the workflow first. Its address is generated when it is created, and is
shown on the workflow afterwards — there is nothing to copy in beforehand, and
the field you would paste it into does not exist until then.

The address is the only thing standing between this endpoint and anybody who
found it, so treat it as a credential: put it wherever the sender keeps its
secrets rather than in a checked-in config file, and rotate it if it leaks.
Rotation gives the workflow a new address and immediately stops the old one
working, so plan to update the sender at the same time.

Configure the sender to `POST` to it with a JSON body. No particular content
type, header or signature is required.

## Writing the title and description

Both are templates. Write `${{ some.path }}` to insert a value from the
delivery's body, addressed by dotted path:

```
[${{ repository.name }}] deployed to ${{ deployment.environment }}
```

A path that is not present renders as nothing rather than failing the delivery,
since a sender is free to omit fields and a missing one should not cost you the
notification. Rendered output is length-capped, so a template aimed at a large
field cannot produce an unbounded task.

## Choosing which deliveries to file

The filter uses the same dotted paths. There are no suggestions to offer here,
because only you know what your sender posts — send yourself one delivery and
look at it before writing this.

```
action == "completed" && conclusion != "success"
```

Two things to know about how JSON maps onto the filter. Arrays become lists, so
membership works:

```
"bug" in issue.labels
```

Objects do not: a path that stops at an object is treated as absent, so filter
on `issue.title` rather than on `issue`. An array of objects is likewise a list
of nothings.

Leave the filter empty to file every delivery.
"#;

crate::register_job!(WebhookTodoistWorkflow);
crate::register_workflow_type!(WebhookTodoistWorkflow);

impl crate::workflows::ConfigurableWorkflow for WebhookTodoistWorkflow {
    type ConfigType = WebhookTodoistConfig;

    fn type_id() -> &'static str {
        "webhook"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Webhook".to_string(),
            description: "Files a task when something posts to this workflow's own URL."
                .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Webhook {
                source: "generic".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(WebhookTodoistConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Deploy notifications".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(WebhookTodoistConfig: title),
                    "Task title",
                    FieldKind::Text {
                        placeholder: Some("Deployed ${{ deployment.environment }}".into()),
                    },
                )
                .with_help(
                    "What the task is called. Write ${{ some.path }} to insert a value from the payload.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(WebhookTodoistConfig: description),
                    "Task description",
                    FieldKind::TextArea {
                        placeholder: Some("Started by ${{ sender.login }}".into()),
                    },
                )
                .with_help("Optional. Written the same way as the title."),
                FieldDescriptor::new(
                    crate::config_path!(WebhookTodoistConfig: filter),
                    "Filter",
                    // Deliberately empty: every other workflow knows the shape
                    // of the things it collected and can suggest their names,
                    // and this one is the workflow for senders we have never
                    // seen. Suggesting anything here would be guessing, so the
                    // help says where the real names come from instead.
                    FieldKind::Filter { fields: vec![] },
                )
                .with_help(
                    "Only file deliveries matching this. Match on the payload's own field names, addressed by path — such as action == \"opened\", or \"bug\" in issue.labels. They differ from one sender to the next, so check what yours actually posts. Leave it empty to file every delivery.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                WebhookTodoistConfig,
                project = Some("Inbox"),
                section = None::<&str>
            ))
            .collect(),
        }
    }
}

impl Job for WebhookTodoistWorkflow {
    type JobType = WebhookTodoistTask;

    fn partition() -> &'static str {
        "webhooks/generic"
    }

    #[instrument("workflow.webhook_todoist.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        let (id, event) = (job.workflow, &job.event);

        // Read now rather than carried in the payload, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(record) = crate::workflow_store::WorkflowStore::new(&services)
            .find(id)
            .await?
        else {
            info!(
                workflow.id = %id,
                "The workflow this delivery was for has been deleted, so it is discarded.",
            );
            return Ok(());
        };

        if !record.enabled {
            info!(
                workflow.id = %id,
                "The workflow this delivery was for is paused, so it is discarded.",
            );
            return Ok(());
        }

        let config: WebhookTodoistConfig = serde_json::from_value(record.config.clone())
            .wrap_user_err(
                format!("The webhook workflow '{id}' is not configured correctly."),
                &["Open this workflow and check that every field it asks for is filled in."],
            )?;

        // A sender posting form encoding or XML has been pointed at the wrong
        // kind of endpoint. That is worth saying out loud, but it is a
        // misconfiguration somebody has to go and fix rather than something a
        // retry could ever resolve, so it does not fail the delivery.
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.body) else {
            warn!(
                workflow.id = %id,
                "Ignoring a webhook delivery whose body is not JSON; this workflow can only read JSON payloads.",
            );
            return Ok(());
        };

        if !config.filter.matches(&JsonFilter(&payload))? {
            debug!(
                workflow.id = %id,
                "A webhook delivery did not match this workflow's filter, so no task was filed.",
            );
            return Ok(());
        }

        let title = render(&config.title, &payload)?;

        let description = match &config.description {
            Some(template) => Some(render(template, &payload)?),
            None => None,
        };

        TodoistCreateTask::dispatch(
            TodoistCreateTaskPayload {
                title,
                description,
                due: TodoistDueDate::Today,
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;

    use crate::workflow_store::{WorkflowDraft, WorkflowStore};

    use super::*;

    /// A configuration that files a task for deployments and ignores the rest.
    fn config() -> serde_json::Value {
        serde_json::json!({
            "name": "Deploys",
            "title": "[${{ repository.name }}] deployed to ${{ deployment.environment }}",
            "filter": "action == \"deployed\"",
        })
    }

    /// A payload the configuration above would file a task for.
    fn body() -> String {
        serde_json::json!({
            "action": "deployed",
            "repository": { "name": "automate" },
            "deployment": { "environment": "production" },
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
                type_id: "webhook".into(),
                config,
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id
    }

    fn delivery(workflow: automate_api::WorkflowId, body: impl Into<String>) -> WebhookTodoistTask {
        WebhookTodoistTask {
            workflow,
            event: WebhookEvent {
                body: body.into(),
                query: String::new(),
                headers: HashMap::new(),
            },
        }
    }

    /// Runs one delivery the way the consumer would.
    async fn run(
        services: &(impl Services + Send + Sync + Clone + 'static),
        task: &WebhookTodoistTask,
    ) -> Result<(), human_errors::Error> {
        WebhookTodoistWorkflow
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                task,
            )
            .await
    }

    /// The tasks this workflow asked to have created.
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
    async fn a_delivery_matching_the_filter_files_a_task_under_its_rendered_title() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery(workflow, body())).await.unwrap();

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0].payload["title"], "[automate] deployed to production",
            "the title should be the user's template with the payload's own values in it",
        );
        assert_eq!(
            filed[0].payload["due"], "Today",
            "a notification that arrives today is about today",
        );
    }

    #[tokio::test]
    async fn a_delivery_the_filter_rejects_files_nothing() {
        // The filter is the only thing standing between a chatty sender and a
        // task per delivery, so a payload it does not match has to produce
        // nothing at all rather than a task with an empty title.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let ignored = serde_json::json!({ "action": "created" }).to_string();
        run(&services, &delivery(workflow, ignored)).await.unwrap();

        assert!(
            filed(&services).await.is_empty(),
            "a delivery the user asked to ignore should not have filed anything",
        );
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_discarded_rather_than_failing_the_delivery() {
        // A sender posting form data has been pointed at the wrong endpoint.
        // Failing here would put the message back on the queue to be retried
        // forever, and no number of retries makes form data into JSON.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(
            &services,
            &delivery(workflow, "action=deployed&environment=production"),
        )
        .await
        .expect("a body we cannot read should not fail the job");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_template_naming_something_the_sender_omitted_still_files_a_task() {
        // Payloads vary between deliveries: an optional field is present one
        // time and absent the next. Losing the whole notification over a gap in
        // its title would turn a cosmetic problem into a dropped alert.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Deploys",
                "title": "Deployed by ${{ sender.login }}",
                "description": "Release ${{ release.tag }}",
            }),
        )
        .await;

        let sparse = serde_json::json!({ "action": "deployed" }).to_string();
        run(&services, &delivery(workflow, sparse)).await.unwrap();

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0].payload["title"], "Deployed by ",
            "the literal text around a missing value should still render",
        );
        assert_eq!(filed[0].payload["description"], "Release ");
    }

    #[tokio::test]
    async fn a_description_is_only_set_when_the_workflow_asked_for_one() {
        // An empty string and no description are different things to Todoist,
        // and a workflow that left the field blank did not ask for either.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery(workflow, body())).await.unwrap();

        assert!(filed(&services).await[0].payload["description"].is_null());
    }

    #[tokio::test]
    async fn a_delivery_for_a_deleted_workflow_stops_there() {
        // Deliveries can be queued behind one another, so a workflow can be
        // deleted while one of its own is still waiting to run.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        WorkflowStore::new(&services)
            .with_index(&services)
            .delete(workflow)
            .await
            .unwrap();

        run(&services, &delivery(workflow, body()))
            .await
            .expect("a deleted workflow should not fail the delivery");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_delivery_for_a_paused_workflow_files_nothing() {
        // Pausing exists so somebody can stop a noisy sender without losing
        // their configuration, which only works if a paused workflow is quiet.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let store_ = WorkflowStore::new(&services).with_index(&services);
        let workflow = store(&services, config()).await;

        store_
            .update(
                workflow,
                WorkflowDraft {
                    type_id: "webhook".into(),
                    config: config(),
                    schedule: None,
                    enabled: false,
                },
            )
            .await
            .unwrap();

        run(&services, &delivery(workflow, body())).await.unwrap();

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_delivery_uses_the_configuration_as_it_stands_now() {
        // The payload names the workflow rather than carrying a copy of it, so
        // that an edit made while a delivery was queued is the one that applies
        // rather than being skipped over exactly once.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        WorkflowStore::new(&services)
            .with_index(&services)
            .update(
                workflow,
                WorkflowDraft {
                    type_id: "webhook".into(),
                    config: serde_json::json!({
                        "name": "Deploys",
                        "title": "Renamed since the delivery arrived",
                    }),
                    schedule: None,
                    enabled: true,
                },
            )
            .await
            .unwrap();

        run(&services, &delivery(workflow, body())).await.unwrap();

        assert_eq!(
            filed(&services).await[0].payload["title"],
            "Renamed since the delivery arrived",
        );
    }

    #[test]
    fn a_stored_configuration_names_the_workflow_it_describes() {
        // What the registry calls this workflow in a list of somebody's
        // workflows, which is the only reason the payload type has to be able
        // to hold a configuration at all.
        let workflow = crate::workflows::lookup("webhook").expect("the type is registered");

        assert_eq!(
            workflow
                .describe(&serde_json::json!({
                    "name": "Deploys",
                    "title": "${{ action }}",
                }))
                .unwrap(),
            "Deploys",
        );
    }

    #[test]
    fn a_configuration_missing_a_required_field_is_refused_by_name() {
        // The registry's error is only useful if serde got far enough to say
        // which field was at fault, which is the whole reason this type is
        // deserialized by hand rather than left untagged.
        let workflow = crate::workflows::lookup("webhook").expect("the type is registered");

        let Err(err) = workflow.validate(&serde_json::json!({ "name": "Deploys" })) else {
            panic!("a configuration with no title should not validate");
        };

        assert!(
            format!("{err}").contains("title"),
            "the error should name the field that is missing: {err}",
        );
    }

    #[test]
    fn the_shape_a_delivery_arrives_in_is_pinned() {
        // The ingress builds this payload from the outside, so the shape it has
        // to write is worth stating here rather than leaving to be inferred.
        let workflow = automate_api::WorkflowId::from_entropy(1);

        let task: WebhookTodoistTask = serde_json::from_value(serde_json::json!({
            "workflow": workflow.to_string(),
            "event": { "body": "{}", "query": "", "headers": {} },
        }))
        .expect("a delivery should deserialize");

        assert_eq!(task.workflow, workflow);
        assert_eq!(task.event.body, "{}");
    }
}
