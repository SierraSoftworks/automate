use serde::{Deserialize, Serialize};

use crate::{
    prelude::*,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate},
    webhooks::WebhookDelivery,
    webhooks::grafana::tokens_match,
};

/// What one person asked us to do with their Honeycomb triggers.
///
/// This carries the shared secret Honeycomb's webhook recipient sends, because
/// the address on its own does not do the job people assumed it did. The address
/// travels in the URL: it is written to reverse-proxy access logs, to
/// Honeycomb's own delivery records, and to anything sitting between the two. It
/// is the part of the request most likely to end up somewhere it should not be.
/// The secret is carried in the `X-Honeycomb-Webhook-Token` header instead,
/// which those places do not record. The two therefore defend against different
/// exposures rather than being two locks on one door, and the secret is what
/// survives a leaked URL.
///
/// One value rather than the list of `trusted_secrets` this used to hold. The
/// list existed so several could be valid at once while somebody rotated them,
/// but there is nothing in the form that collects a list, so it would have had
/// to be a comma-separated text box — a parsing convention hiding behind a
/// control that says "text". A recipient in Honeycomb has one secret anyway, so
/// rotating it is one edit on each side; the cost is the seconds between the two
/// saves, during which deliveries are refused and logged. That is a small,
/// visible, deliberate outage, and it buys a field that behaves the same way as
/// the one on every other webhook type here.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct HoneycombWebhookConfig {
    /// What to call this workflow, so that somebody with triggers from two
    /// Honeycomb environments can tell which of them filed a task.
    pub name: String,

    /// The shared secret set on the Honeycomb webhook recipient, checked against
    /// the `X-Honeycomb-Webhook-Token` header on each delivery. Deliveries are
    /// refused while this is unset — see [`HoneycombWebhook::handle`] for why.
    #[serde(default)]
    pub secret: String,

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

impl HoneycombWebhook {
    /// HTTP header names are case-insensitive, and what reaches us depends on
    /// whatever proxy handled the request, so the lookup cannot assume a casing.
    fn header<'a>(event: &'a WebhookEvent, name: &str) -> Option<&'a str> {
        event
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Files a Todoist task when a Honeycomb trigger fires, linking to the query
result so you can go and look at the traces that caused it. Tasks are raised at
the highest priority, on the assumption that a trigger you configured is a
trigger you meant.

Only the firing edge is acted on: Honeycomb also posts when a trigger returns
to normal, and those deliveries are ignored rather than completing anything.
This type files tasks and never closes them.

## Getting the address

Save the workflow first. Its address is generated when it is created and shown
on the workflow afterwards; there is nothing to paste into Honeycomb until
then.

Then, in Honeycomb:

1. Under your team's **Integrations** settings, add a **Webhook** recipient.
   Give it a name you will recognise and set its URL to this workflow's
   address.
2. Fill in that recipient's **Shared Secret** — see below.
3. Open each trigger you want here and add that recipient to it.

A recipient with no triggers attached delivers nothing, which is the usual
reason a freshly configured workflow stays quiet.

## The shared secret

Generate a long random string, put it in the recipient's **Shared Secret**
field, and paste the same value into **Shared secret** here. They have to match
exactly: Honeycomb sends it on every delivery in the
`X-Honeycomb-Webhook-Token` header and we check it against our copy.

This is not a second lock on the same door as the address. The address travels
in the URL, so it is written into reverse-proxy access logs, into Honeycomb's
own delivery records, and into anything sitting between the two — it is the
part of the request most likely to end up somewhere it should not be. The
secret rides in a header, which those places do not keep. A leaked address
therefore does not leak the secret, and the secret still refuses the delivery.

**A workflow with no secret refuses every trigger.** An empty field is not
treated as "skip the check" — that would leave a workflow silently
unauthenticated at exactly the moment somebody forgot to finish setting it up,
so a missing secret fails closed instead. It also means the check cannot be
turned off by clearing the box.

Only one secret is held here, so rotating it means changing both sides. Change
it in Honeycomb, then here; deliveries that land between the two are refused
and logged, which is a few seconds of quiet rather than anything lost — a
Honeycomb trigger that is still firing will fire again.

## Choosing which triggers to file

The filter runs against each trigger and can match on `id` and `name` — the
trigger's identifier and its display name as Honeycomb sends them. That is a
deliberately small surface; if you need more than this, use several recipients
in Honeycomb instead.

```
name startswith "prod:"
```

Leave it empty to file every trigger that fires, which is the sensible default
when the recipient is only attached to triggers you chose.
"#;

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
            documentation: DOCUMENTATION.to_string(),
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
                    crate::config_path!(HoneycombWebhookConfig: secret),
                    "Shared secret",
                    FieldKind::Text {
                        placeholder: Some("a long random string".into()),
                    },
                )
                .with_help(
                    "The Shared Secret set on the webhook recipient in Honeycomb's Integrations settings. It has to be the same string on both sides. Honeycomb sends it in a header, which logs and delivery histories do not keep, whereas the address travels in the URL where they do — so this is what still refuses a delivery somebody sent because they found the address. Triggers are refused while this is empty.",
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

        // Everything below this point happens *before* the payload is parsed, so
        // that a delivery we cannot attribute to Honeycomb is never interpreted,
        // let alone acted on. The payload carries a `shared_secret` of its own,
        // which is deliberately not what is checked: believing the body about
        // whether the body can be believed proves nothing.
        //
        // A rejection returns `Ok(())` rather than an error: nothing about a
        // wrong secret improves by trying again, so raising here would only leave
        // the delivery retrying forever and hiding real failures behind it. The
        // log line is the record that it happened.

        // No secret configured means we refuse, rather than accept anything. The
        // alternative — treating an empty secret as "skip the check" — would make
        // a workflow silently unauthenticated exactly when somebody forgot to
        // finish setting it up, and a forgotten field should fail closed. It also
        // means the check cannot be neutralised by clearing the box, and it is
        // what the GitHub and Terraform Cloud webhooks do with their own.
        if config.secret.is_empty() {
            warn!(
                "Received a Honeycomb webhook for a workflow with no shared secret configured; rejecting request."
            );
            return Ok(());
        }

        let Some(presented) = Self::header(&job.event, "x-honeycomb-webhook-token") else {
            warn!(
                "Received a Honeycomb webhook without an X-Honeycomb-Webhook-Token header; rejecting request."
            );
            return Ok(());
        };

        if !tokens_match(&config.secret, presented) {
            warn!(
                "Received a Honeycomb webhook whose X-Honeycomb-Webhook-Token did not match the configured secret; rejecting request."
            );
            return Ok(());
        }

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

    /// The secret these tests pretend was set on both this workflow and the
    /// Honeycomb webhook recipient.
    const SECRET: &str = "a-long-random-string";

    /// A workflow authorised with [`SECRET`].
    fn config() -> serde_json::Value {
        serde_json::json!({ "name": "Production", "secret": SECRET })
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

    /// A delivery carrying the token Honeycomb would have sent.
    fn delivery(workflow: automate_api::WorkflowId, body: impl Into<String>) -> WebhookDelivery {
        delivery_with(workflow, body, &[("X-Honeycomb-Webhook-Token", SECRET)])
    }

    /// A delivery carrying whatever headers the test wants, for the ones about
    /// what happens when the token is wrong, absent or unmatched.
    fn delivery_with(
        workflow: automate_api::WorkflowId,
        body: impl Into<String>,
        headers: &[(&str, &str)],
    ) -> WebhookDelivery {
        WebhookDelivery {
            workflow,
            event: WebhookEvent {
                body: body.into(),
                query: String::new(),
                headers: headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<HashMap<_, _>>(),
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
        let workflow = store(&services, config()).await;

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
        let workflow = store(&services, config()).await;

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
                "secret": SECRET,
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
        let workflow = store(&services, config()).await;

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

    #[tokio::test]
    async fn a_trigger_carrying_the_wrong_secret_files_nothing() {
        // Each workflow carries its own secret, so a delivery authorised for
        // somebody else's must not be acted on by this one.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let job = delivery_with(
            workflow,
            triggered_payload(),
            &[("X-Honeycomb-Webhook-Token", "somebody-elses-secret")],
        );

        run(&services, &job)
            .await
            .expect("a misauthorised trigger should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_trigger_with_no_token_header_at_all_files_nothing() {
        // Anybody can post to a URL, and the URL is the part of the request most
        // likely to have leaked. Without the header there is nothing to check,
        // and "nothing to check" is not the same as "checks out".
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(
            &services,
            &delivery_with(workflow, triggered_payload(), &[]),
        )
        .await
        .expect("an unauthorised trigger should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn the_secret_in_the_body_is_not_what_is_checked() {
        // Honeycomb also puts a `shared_secret` in the payload. Believing the
        // body about whether the body can be believed proves nothing, so a
        // delivery that names the right secret in the payload and carries no
        // header is still refused.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let body = serde_json::json!({
            "version": "v0.1.0",
            "shared_secret": SECRET,
            "name": "Slow requests",
            "id": "abc123",
            "status": "TRIGGERED",
            "summary": "p99 latency is above 500ms",
            "operator": ">",
            "threshold": 500.0,
        })
        .to_string();

        run(&services, &delivery_with(workflow, body, &[]))
            .await
            .expect("a trigger vouching for itself should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_trigger_carrying_a_near_miss_of_the_secret_files_nothing() {
        // A near miss is the interesting case: somebody probing for the secret
        // works by getting closer to it, so a value that shares a long prefix
        // with the real one, or differs only in case or length, has to be as
        // rejected as a value that shares nothing.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        for near_miss in [
            &SECRET[..SECRET.len() - 1],
            &format!("{SECRET}x"),
            &SECRET.to_uppercase(),
            "",
        ] {
            let job = delivery_with(
                workflow,
                triggered_payload(),
                &[("X-Honeycomb-Webhook-Token", near_miss)],
            );

            run(&services, &job)
                .await
                .expect("a near miss should be refused without erroring");

            assert!(
                filed(&services).await.is_empty(),
                "'{near_miss}' is not the configured secret and must not be treated as it",
            );
        }
    }

    #[tokio::test]
    async fn triggers_are_refused_while_the_workflow_has_no_secret_configured() {
        // A half-finished workflow should file nothing rather than file whatever
        // anybody who found the URL cares to post. An empty field means "cannot
        // be verified", not "need not be verified" — and in particular an empty
        // secret must not match an empty header.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({ "name": "Production", "secret": "" }),
        )
        .await;

        run(&services, &delivery(workflow, triggered_payload()))
            .await
            .expect("an unverifiable trigger should be refused without erroring");
        run(
            &services,
            &delivery_with(
                workflow,
                triggered_payload(),
                &[("X-Honeycomb-Webhook-Token", "")],
            ),
        )
        .await
        .expect("an empty header should not satisfy an empty secret");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn the_token_header_is_recognised_whatever_case_it_arrives_in() {
        // HTTP header names are case-insensitive and whatever proxy sits in
        // front of us is free to renormalise them, so a lowercase header must
        // not read as a missing one.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let job = delivery_with(
            workflow,
            triggered_payload(),
            &[("x-honeycomb-webhook-token", SECRET)],
        );

        run(&services, &job)
            .await
            .expect("a correctly authorised trigger should be processed");

        assert_eq!(filed(&services).await.len(), 1);
    }
}
