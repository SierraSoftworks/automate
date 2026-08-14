use std::fmt::Display;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{
    prelude::*,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate},
};

type HmacSha256 = Hmac<Sha256>;

/// What one person asked us to do with the events their tailnet reports.
///
/// This carries a shared secret, because the address on its own does not do the
/// job people assumed it did. The address travels in the URL: it is written to
/// reverse-proxy access logs, to Tailscale's own delivery history, and to
/// anything sitting between the two. It is the part of the request most likely
/// to end up somewhere it should not be. Tailscale's
/// `Tailscale-Webhook-Signature` is an HMAC over the body carried in a *header*,
/// which those places do not record — this installation's own tracing redacts
/// credential-bearing headers (see [`crate::web::telemetry`]) — and it
/// additionally proves the body was not rewritten on the way. The two therefore
/// defend against different exposures rather than being two locks on one door,
/// and the signature is what survives a leaked URL.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TailscaleWebhookConfig {
    /// What to call this workflow, so it can be told apart from the others in a
    /// list of them.
    pub name: String,

    /// The secret Tailscale generated for this webhook endpoint, used to verify
    /// the `Tailscale-Webhook-Signature` HMAC. Deliveries are refused while this
    /// is unset — see [`TailscaleWebhook::handle`] for why.
    #[serde(default)]
    pub secret: String,

    #[serde(default)]
    pub filter: crate::filter::Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: crate::publishers::TodoistTarget,
}

impl Display for TailscaleWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tailscale/{}", self.name)
    }
}

fn default_todoist_config() -> crate::publishers::TodoistTarget {
    crate::publishers::TodoistTarget {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

#[derive(Clone)]
pub struct TailscaleWebhook;

impl TailscaleWebhook {
    /// Parses Tailscale's `t=<unix-seconds>,v1=<hex>` signature header into its
    /// timestamp and raw bytes.
    fn parse_signature(header: &str) -> Result<(DateTime<Utc>, Vec<u8>), human_errors::Error> {
        let mut timestamp = None;
        let mut signature = None;

        for (key, value) in header.split(',').filter_map(|s| s.split_once('=')) {
            match key {
                "t" => timestamp = Some(value),
                "v1" => signature = Some(value),
                _ => {} // Ignore unknown fields
            }
        }

        match (timestamp, signature) {
            (Some(timestamp), Some(signature)) => {
                let timestamp = timestamp
                    .parse()
                    .ok()
                    .and_then(|ts| DateTime::from_timestamp(ts, 0))
                    .ok_or_else(|| {
                        human_errors::user(
                            "The timestamp in the Tailscale-Webhook-Signature header is invalid.",
                            &[
                                "Ensure that you are only sending Tailscale webhooks to this endpoint.",
                                "Check that the webhook is configured correctly at https://login.tailscale.com/admin/settings/webhooks",
                            ],
                        )
                    })?;

                let signature = hex::decode(signature).or_user_err(&[
                    "The signature in the Tailscale-Webhook-Signature header is not valid hex.",
                    "Ensure that you are only sending Tailscale webhooks to this endpoint.",
                    "Check that the webhook is configured correctly at https://login.tailscale.com/admin/settings/webhooks",
                ])?;

                Ok((timestamp, signature))
            }
            _ => Err(human_errors::user(
                "The Tailscale-Webhook-Signature header did not contain a valid signature.",
                &[
                    "Ensure that you are only sending Tailscale webhooks to this endpoint.",
                    "Check that the webhook is configured correctly at https://login.tailscale.com/admin/settings/webhooks",
                ],
            )),
        }
    }

    /// Verifies the Tailscale webhook signature.
    ///
    /// According to https://tailscale.com/kb/1213/webhooks#verifying-an-event-signature,
    /// Tailscale signs webhooks using HMAC-SHA256 over `"<timestamp>.<body>"` with
    /// the webhook secret, and carries the digest in the
    /// `Tailscale-Webhook-Signature` header as `t=<timestamp>,v1=<hex_signature>`.
    ///
    /// The `now` parameter is the point in time against which the signature
    /// timestamp is validated. This should be the time at which the request was
    /// originally received (rather than the current time) so that retries of a
    /// previously received request continue to validate successfully.
    fn verify_signature(
        secret: &str,
        body: &str,
        signature_header: &str,
        now: DateTime<Utc>,
    ) -> Result<(), human_errors::Error> {
        let (timestamp, expected_signature) = Self::parse_signature(signature_header)?;

        if (timestamp - now).abs() > chrono::Duration::minutes(5) {
            return Err(human_errors::user(
                format!(
                    "The Tailscale webhook signature timestamp is too old or too far in the future (got {timestamp})"
                ),
                &[
                    "Ensure that the system clock on this server is accurate.",
                    "Check that the webhook is configured correctly at https://login.tailscale.com/admin/settings/webhooks",
                ],
            ));
        }

        // The timestamp is inside the signed material, so a replayed delivery
        // cannot be re-dated to slip past the window above.
        let string_to_sign = format!("{}.{}", timestamp.timestamp(), body);

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).wrap_user_err(
            "Failed to create HMAC instance with the provided secret.",
            &["Ensure that you have set a valid webhook secret on this workflow."],
        )?;

        mac.update(string_to_sign.as_bytes());

        // `verify_slice` compares in constant time, so a wrong signature cannot
        // be walked one byte at a time by timing the rejections.
        mac.verify_slice(&expected_signature).wrap_user_err(
            "Webhook signature verification failed (signatures did not match).".to_string(),
            &[
                "Ensure that the webhook secret on this workflow matches the one shown against the endpoint at https://login.tailscale.com/admin/settings/webhooks",
            ],
        )?;

        Ok(())
    }

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

Files a Todoist task for each event your tailnet reports, using Tailscale's own
wording as the title and quoting the event's payload into the body so it can be
triaged without going to look. Tailscale batches events, so one delivery can
produce several tasks.

Priority follows what the event means for you. Things that block somebody — a
node or user waiting for approval, an expired node key, IP forwarding that is
not enabled — are raised urgently. Policy updates, new nodes and user changes
are filed normally. Deletions and webhook changes lower still.

## Getting the address

Save the workflow first. Its address is generated when it is created and shown
on the workflow afterwards; there is nothing to paste into Tailscale until
then.

Then, in the [Tailscale admin console](https://login.tailscale.com/admin), open
**Settings → Webhooks** and add a webhook endpoint:

1. Set the endpoint URL to this workflow's address.
2. Choose the event types to subscribe to. Only what you subscribe to is
   delivered, so this is the coarse filter and the one below is the fine one.

Tailscale's
[webhook documentation](https://tailscale.com/kb/1213/webhooks) lists every
event type and what its payload contains.

## The webhook secret

Once the endpoint is created, Tailscale shows a secret against it on that same
**Settings → Webhooks** page. Copy it into **Webhook secret** here. Tailscale
signs every delivery with it, sending an HMAC of the body in the
`Tailscale-Webhook-Signature` header, and we check that against our copy — so
the two have to match. Rotating it in Tailscale means pasting the new value
here as well.

This is not a second lock on the same door as the address. The address travels
in the URL, so it is written into reverse-proxy access logs, into Tailscale's
own delivery records, and into anything sitting between the two — it is the
part of the request most likely to end up somewhere it should not be. The
secret never appears in the URL, only in a header, so a leaked address does not
leak it. The signature also covers the body, which the address cannot: it
proves nothing rewrote the event on the way.

**A workflow with no secret refuses every delivery.** An empty field is not
treated as "skip the check" — that would leave a workflow silently
unauthenticated at exactly the moment somebody forgot to finish setting it up,
so a missing secret fails closed instead. It also means the check cannot be
turned off by clearing the box.

## Choosing which events to file

The filter runs against each event in a delivery and can match on `type`,
`tailnet` and `message`. `type` is the event name exactly as Tailscale spells
it — `nodeNeedsApproval`, `policyUpdate`, `nodeKeyExpired` and so on.

```
type in ["nodeNeedsApproval", "userNeedsApproval", "nodeKeyExpired"]
```

Leave it empty to file every event this webhook is subscribed to. Tailscale
sends a `test` event when you create the endpoint, which is a quick way to
confirm the address is right before you narrow the filter.
"#;

crate::register_job!(TailscaleWebhook);
crate::register_workflow_type!(TailscaleWebhook);

impl crate::workflows::ConfigurableWorkflow for TailscaleWebhook {
    type ConfigType = TailscaleWebhookConfig;

    fn type_id() -> &'static str {
        "tailscale"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Tailscale".to_string(),
            description: "Files a task when your tailnet reports something that needs a person."
                .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Webhook {
                source: "tailscale".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(TailscaleWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Tailnet alerts".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(TailscaleWebhookConfig: secret),
                    "Webhook secret",
                    // No generator: Tailscale issues this one, so anything we
                    // made up here could only ever be the wrong value.
                    FieldKind::Secret {
                        placeholder: Some("tskey-webhook-…".into()),
                        generator: false,
                        generator_bytes: 32,
                    },
                )
                .with_help(
                    "The secret Tailscale shows against this endpoint under Settings → Webhooks. Tailscale signs the body of every delivery with it, which is what proves the delivery came from your tailnet and was not rewritten on the way — the address alone cannot say either, and it travels in the URL where logs can see it. It must be the same value on both sides, and events are refused while this is empty.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(TailscaleWebhookConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec!["type".into(), "tailnet".into(), "message".into()],
                    },
                )
                .with_help(
                    "Only file the events matching this, such as type == \"nodeNeedsApproval\". Leave it empty to file every event this webhook is subscribed to.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                TailscaleWebhookConfig,
                project = Some("Life"),
                section = Some("Tasks & Chores")
            ))
            .collect(),
        }
    }
}

impl Job for TailscaleWebhook {
    type JobType = crate::webhooks::WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/tailscale"
    }

    #[instrument("webhooks.tailscale.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the delivery, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<TailscaleWebhookConfig>(services).await? else {
            return Ok(());
        };

        let event = &job.event;

        // Everything below this point happens *before* the payload is parsed, so
        // that a delivery we cannot attribute to Tailscale is never interpreted,
        // let alone acted on.
        //
        // A rejection returns `Ok(())` rather than an error: nothing about a bad
        // signature improves by trying again, so raising here would only leave
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
                "Received a Tailscale webhook for a workflow with no secret configured; rejecting request."
            );
            return Ok(());
        }

        let Some(signature) = Self::header(event, "tailscale-webhook-signature") else {
            warn!(
                "Received a Tailscale webhook without a Tailscale-Webhook-Signature header; rejecting request."
            );
            return Ok(());
        };

        // Validate against the time the request was originally received (the
        // message's scheduled time) rather than now, so that a retry of a
        // delivery we already accepted still validates.
        if let Err(err) =
            Self::verify_signature(&config.secret, &event.body, signature, ctx.scheduled_at())
        {
            warn!(
                "Failed to verify Tailscale webhook signature, rejecting request: {}",
                err
            );
            return Ok(());
        }

        // Tailscale delivers webhook events as a JSON array, even when only a
        // single event is included. https://tailscale.com/kb/1213/webhooks
        let events: Vec<TailscaleAlertEventPayload> = event.json()?;

        for event in events {
            if !config.filter.matches(&event)? {
                info!(
                    "Tailscale event '{}' did not match filter; ignoring.",
                    event._type
                );
                continue;
            }

            let pretty_payload = serde_json::to_string_pretty(&event.data)
                .unwrap_or_else(|_| job.event.body.clone());

            TodoistCreateTask::dispatch(
                TodoistCreateTaskPayload {
                    title: format!(
                        "[**Tailscale**](https://login.tailscale.com/admin): {}",
                        event.message
                    ),
                    description: Some(format!("```\n{pretty_payload}\n```")),
                    due: TodoistDueDate::DateTime(event.timestamp),
                    priority: Some(match event._type.as_str() {
                        "exitNodeIPForwardingNotEnabled" => 4,
                        "subnetIPForwardingNotEnabled" => 4,
                        "nodeNeedsApproval" => 4,
                        "nodeKeyExpired" => 4,
                        "userNeedsApproval" => 4,

                        "policyUpdate" => 3,
                        "nodeCreated" => 3,
                        "nodeApproved" => 3,
                        "nodeKeyExpiringInOneDay" => 3,
                        "userCreated" => 3,
                        "userApproved" => 3,
                        "userRoleUpdated" => 3,

                        "nodeDeleted" => 2,
                        "webhookUpdated" => 2,
                        "webhookDeleted" => 2,

                        "test" => 1,

                        _ => 3,
                    }),
                    config: config.todoist.clone(),
                    ..Default::default()
                },
                None,
                services,
            )
            .await?;
        }

        Ok(())
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct TailscaleAlertEventPayload {
    version: u32,
    timestamp: DateTime<chrono::Utc>,
    #[serde(rename = "type")]
    _type: String,
    tailnet: String,
    message: String,
    data: serde_json::Value,
}

impl Filterable for TailscaleAlertEventPayload {
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        match key {
            "type" => self._type.as_str().into(),
            "tailnet" => self.tailnet.as_str().into(),
            "message" => self.message.as_str().into(),
            _ => crate::filter::FilterValue::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;

    use crate::{
        webhooks::{WebhookDelivery, WebhookEvent},
        workflow_store::{WorkflowDraft, WorkflowStore},
    };

    use super::*;

    /// A delivery as Tailscale sends it: an array, even for a single event.
    const POLICY_UPDATE: &str = r#"[{"timestamp":"2026-06-19T21:12:52.923385657Z","version":1,"type":"policyUpdate","tailnet":"example.org.github","message":"Tailnet policy file updated","data":{"url":"https://login.tailscale.com/admin/acls"}}]"#;

    /// The secret these tests pretend Tailscale showed against the endpoint.
    const SECRET: &str = "tskey-webhook-a-long-random-string";

    /// Signs a body the way Tailscale does: `t=<unix-seconds>,v1=<hex>` over
    /// `"<timestamp>.<body>"`.
    fn sign(secret: &str, timestamp: i64, body: &str) -> String {
        let string_to_sign = format!("{timestamp}.{body}");
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(string_to_sign.as_bytes());
        format!(
            "t={},v1={}",
            timestamp,
            hex::encode(mac.finalize().into_bytes())
        )
    }

    /// A configuration that files every event the tailnet reports.
    fn config() -> serde_json::Value {
        serde_json::json!({ "name": "Tailnet alerts", "secret": SECRET })
    }

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "tailscale".into(),
                config,
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id
    }

    /// A delivery carrying the signature Tailscale itself would have sent for
    /// it, dated now so it falls inside the freshness window [`run`] checks
    /// against.
    fn delivery(workflow: automate_api::WorkflowId, body: &str) -> WebhookDelivery {
        let signature = sign(SECRET, Utc::now().timestamp(), body);
        delivery_with(
            workflow,
            body,
            &[("Tailscale-Webhook-Signature", &signature)],
        )
    }

    /// A delivery carrying whatever headers the test wants, for the ones about
    /// what happens when the signature is wrong, absent or unmatched.
    fn delivery_with(
        workflow: automate_api::WorkflowId,
        body: &str,
        headers: &[(&str, &str)],
    ) -> WebhookDelivery {
        WebhookDelivery {
            workflow,
            event: WebhookEvent {
                body: body.to_string(),
                query: String::new(),
                headers: headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<HashMap<_, _>>(),
            },
        }
    }

    /// Runs one delivery the way the consumer would.
    async fn run(
        services: &(impl Services + Send + Sync + Clone + 'static),
        delivery: &WebhookDelivery,
    ) -> Result<(), human_errors::Error> {
        TailscaleWebhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                delivery,
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
    async fn an_event_files_a_task_carrying_tailscales_own_wording() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery(workflow, POLICY_UPDATE))
            .await
            .unwrap();

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0].payload["title"],
            "[**Tailscale**](https://login.tailscale.com/admin): Tailnet policy file updated",
        );
        assert!(
            filed[0].payload["description"]
                .as_str()
                .unwrap()
                .contains("https://login.tailscale.com/admin/acls"),
            "the event's own payload should be quoted into the task, so it can be triaged without going to look",
        );
    }

    #[tokio::test]
    async fn a_node_waiting_for_approval_outranks_a_policy_change() {
        // Somebody is blocked until a node is approved, whereas a policy update
        // is a note to self, and a task list that cannot tell them apart is a
        // task list nobody reads top-down.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let needs_approval = POLICY_UPDATE.replace("policyUpdate", "nodeNeedsApproval");
        run(&services, &delivery(workflow, &needs_approval))
            .await
            .unwrap();

        assert_eq!(filed(&services).await[0].payload["priority"], 4);
    }

    #[tokio::test]
    async fn an_event_the_filter_rejects_files_nothing() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Approvals only",
                "secret": SECRET,
                "filter": r#"type == "nodeNeedsApproval""#,
            }),
        )
        .await;

        run(&services, &delivery(workflow, POLICY_UPDATE))
            .await
            .unwrap();

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn every_event_in_one_delivery_gets_its_own_task() {
        // Tailscale batches, so a delivery that only produced one task would
        // quietly drop everything after the first.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let batch = format!(
            "[{},{}]",
            &POLICY_UPDATE[1..POLICY_UPDATE.len() - 1],
            POLICY_UPDATE[1..POLICY_UPDATE.len() - 1].replace("policyUpdate", "nodeCreated"),
        );

        run(&services, &delivery(workflow, &batch)).await.unwrap();

        assert_eq!(filed(&services).await.len(), 2);
    }

    #[tokio::test]
    async fn a_delivery_for_a_paused_workflow_files_nothing() {
        // Pausing exists so somebody can silence a chatty tailnet without losing
        // their configuration, which only works if a paused workflow is quiet.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        WorkflowStore::new(&services)
            .with_index(&services)
            .update(
                workflow,
                WorkflowDraft {
                    type_id: "tailscale".into(),
                    config: config(),
                    schedule: None,
                    enabled: false,
                },
            )
            .await
            .unwrap();

        run(&services, &delivery(workflow, POLICY_UPDATE))
            .await
            .unwrap();

        assert!(filed(&services).await.is_empty());
    }

    #[test]
    fn a_stored_configuration_names_the_workflow_it_describes() {
        let workflow = crate::workflows::lookup("tailscale").expect("the type is registered");

        assert_eq!(
            workflow
                .describe(&serde_json::json!({ "name": "Tailnet alerts" }))
                .unwrap(),
            "Tailnet alerts",
        );
    }

    #[test]
    fn test_parse_event_array() {
        // Tailscale delivers events as a JSON array, even for a single event.
        let events: Vec<TailscaleAlertEventPayload> =
            serde_json::from_str(POLICY_UPDATE).expect("array payload should parse");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0]._type, "policyUpdate");
        assert_eq!(events[0].tailnet, "example.org.github");
        assert_eq!(events[0].message, "Tailnet policy file updated");
    }

    #[test]
    fn a_delivery_signed_with_the_configured_secret_is_accepted() {
        let now = Utc::now();

        TailscaleWebhook::verify_signature(
            SECRET,
            POLICY_UPDATE,
            &sign(SECRET, now.timestamp(), POLICY_UPDATE),
            now,
        )
        .expect("a signature Tailscale itself would have produced should verify");
    }

    #[test]
    fn a_delivery_signed_with_a_different_secret_is_refused() {
        // Knowing the URL is not knowing the secret, which is the whole point of
        // checking one.
        let now = Utc::now();
        let signature = sign("somebody-elses-secret", now.timestamp(), POLICY_UPDATE);

        assert!(
            TailscaleWebhook::verify_signature(SECRET, POLICY_UPDATE, &signature, now).is_err()
        );
    }

    #[test]
    fn a_body_altered_after_signing_no_longer_matches_its_signature() {
        // The signature covers the body, so replaying a genuine signature over a
        // payload somebody rewrote in transit has to fail — otherwise a leaked
        // URL would let anybody claim any node was waiting for approval.
        let now = Utc::now();
        let signature = sign(SECRET, now.timestamp(), POLICY_UPDATE);
        let tampered = POLICY_UPDATE.replace("policyUpdate", "nodeNeedsApproval");

        assert!(TailscaleWebhook::verify_signature(SECRET, &tampered, &signature, now).is_err());
    }

    #[test]
    fn a_signature_header_in_some_other_shape_is_refused() {
        // Tailscale sends `t=…,v1=…`; anything else is not a signature we failed
        // to match, it is a header we cannot even read.
        let now = Utc::now();

        assert!(
            TailscaleWebhook::verify_signature(SECRET, POLICY_UPDATE, "not-a-signature", now)
                .is_err()
        );
        assert!(
            TailscaleWebhook::verify_signature(SECRET, POLICY_UPDATE, "t=1663781880", now).is_err(),
            "a header with a timestamp but no digest proves nothing",
        );
        assert!(
            TailscaleWebhook::verify_signature(
                SECRET,
                POLICY_UPDATE,
                "v1=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                now,
            )
            .is_err(),
            "a digest with no timestamp cannot be checked for freshness",
        );
    }

    #[test]
    fn a_signature_is_checked_against_when_the_delivery_arrived_not_when_it_is_retried() {
        // A delivery that failed and was requeued is verified again hours later.
        // Checking its timestamp against the current time would reject it for
        // being stale, so a retry that would have succeeded first time round
        // would fail forever.
        let received_at = Utc::now() - chrono::Duration::hours(6);
        let signature = sign(SECRET, received_at.timestamp(), POLICY_UPDATE);

        assert!(
            TailscaleWebhook::verify_signature(SECRET, POLICY_UPDATE, &signature, Utc::now())
                .is_err(),
            "the freshness window should reject a six-hour-old signature against the current time",
        );
        TailscaleWebhook::verify_signature(SECRET, POLICY_UPDATE, &signature, received_at)
            .expect("the same signature should verify against the time it was received");
    }

    #[tokio::test]
    async fn a_delivery_signed_with_the_wrong_secret_files_nothing() {
        // Each workflow carries its own secret, so a delivery signed for
        // somebody else's must not be acted on by this one.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let signature = sign(
            "somebody-elses-secret",
            Utc::now().timestamp(),
            POLICY_UPDATE,
        );
        let job = delivery_with(
            workflow,
            POLICY_UPDATE,
            &[("Tailscale-Webhook-Signature", &signature)],
        );

        run(&services, &job)
            .await
            .expect("a mis-signed delivery should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_delivery_whose_body_was_altered_after_signing_files_nothing() {
        // The signature is what makes the payload trustworthy, so a body that
        // was rewritten between Tailscale signing it and us receiving it must
        // never reach the parser.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let signature = sign(SECRET, Utc::now().timestamp(), POLICY_UPDATE);
        let job = delivery_with(
            workflow,
            &POLICY_UPDATE.replace("policyUpdate", "nodeNeedsApproval"),
            &[("Tailscale-Webhook-Signature", &signature)],
        );

        run(&services, &job)
            .await
            .expect("a tampered delivery should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_delivery_with_no_signature_header_at_all_files_nothing() {
        // Anybody can post to a URL, and the URL is the part of the request most
        // likely to have leaked. Without the header there is nothing to check,
        // and "nothing to check" is not the same as "checks out".
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery_with(workflow, POLICY_UPDATE, &[]))
            .await
            .expect("an unsigned delivery should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn deliveries_are_refused_while_the_workflow_has_no_secret_configured() {
        // A half-finished workflow should file nothing rather than file whatever
        // anybody who found the URL cares to post. An empty field means "cannot
        // be verified", not "need not be verified".
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({ "name": "Tailnet alerts", "secret": "" }),
        )
        .await;

        run(&services, &delivery(workflow, POLICY_UPDATE))
            .await
            .expect("an unverifiable delivery should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn the_signature_header_is_recognised_whatever_case_it_arrives_in() {
        // HTTP header names are case-insensitive and whatever proxy sits in
        // front of us is free to renormalise them, so a lowercase header must
        // not read as a missing one.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let signature = sign(SECRET, Utc::now().timestamp(), POLICY_UPDATE);
        let job = delivery_with(
            workflow,
            POLICY_UPDATE,
            &[("tailscale-webhook-signature", &signature)],
        );

        run(&services, &job)
            .await
            .expect("a correctly signed delivery should be processed");

        assert_eq!(filed(&services).await.len(), 1);
    }
}
