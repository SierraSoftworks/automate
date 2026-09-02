//! Webhook handler for [Grey](https://github.com/SierraSoftworks/grey) state-change notifications.
//!
//! Grey delivers a JSON document whenever a probe or cron changes state. Rather than
//! surfacing every transition immediately, we classify each event through the reusable
//! [`crate::services::debounce`] detector — keyed by a stable `grey/<type>/<name>` key and
//! backed by the [`GREY_FAILURES_PARTITION`] table — and map its [`Detection`] onto Todoist actions
//! so a single task tells a coherent story from unhealthy, through recovering, to recovered:
//!
//! * [`Triggered`](Detection::Triggered) — the monitor is unhealthy. For a brand-new incident (the
//!   detection's `first_triggered_at` is the observation time) we schedule the operator's Todoist
//!   task [`GreyWebhookConfig::alert_delay`] into the future rather than creating it immediately; if
//!   the monitor recovers before the delay elapses the pending task is purged, so a brief blip never
//!   surfaces. When `first_triggered_at` is earlier the monitor has flapped back to unhealthy while
//!   recovering, so we re-escalate the task immediately (dated to the incident's first trigger)
//!   since an operator is already watching it.
//! * [`Recovering`](Detection::Recovering) — the monitor recovered after being triggered. If the
//!   recovery arrives before the alert delay has elapsed the operator's task never
//!   surfaced (its upsert is still pending), so we purge that pending alert and surface nothing — a
//!   brief blip never becomes a task. Otherwise the task has surfaced, so we immediately flip it to
//!   *recovering* at a reduced priority and defer a *recovered* update by the recovery delay,
//!   stamped with the total triggered duration. (Grey already debounces recovery internally for 5m,
//!   so a healthy report is a strong signal.) Any later trigger cancels that deferred update, so the
//!   recovery is only confirmed while it stays newer than the last trigger. Either way the debounce
//!   state is retained, so a relapse within the recovery window is still recognised as the same
//!   flapping incident and re-escalated immediately.

use std::fmt::Display;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{
    prelude::*,
    publishers::{
        TodoistCompleteTask, TodoistCompleteTaskPayload, TodoistDueDate, TodoistUpsertTask,
        TodoistUpsertTaskPayload,
    },
    services::debounce::{DebounceConfig, Debouncer, Detection},
};

type HmacSha256 = Hmac<Sha256>;

/// The key/value partition holding each Grey monitor's debounce state
/// ([`crate::services::debounce::DebounceState`]), keyed by [`GreyWebhookEvent::unique_key`].
const GREY_FAILURES_PARTITION: &str = "grey/failures";

/// Formats a duration as a compact, human-readable string (e.g. `1h 5m`, `12m`, `45s`, `0s`).
/// Sub-minute components are only shown when the duration is under an hour, keeping longer spans
/// tidy. Negative durations are clamped to `0s`.
fn format_duration(duration: chrono::Duration) -> String {
    let total_seconds = duration.num_seconds().max(0);
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 && hours == 0 {
        parts.push(format!("{seconds}s"));
    }
    if parts.is_empty() {
        parts.push("0s".to_string());
    }

    parts.join(" ")
}

/// What one person asked us to do with the state changes their Grey reports.
///
/// This carries a shared secret, because the address on its own does not do the
/// job people assumed it did. The address travels in the URL: it is written to
/// reverse-proxy access logs, to Grey's own delivery history, and to whatever
/// sits between the two. It is the part of the request most likely to be
/// somewhere it should not be. Grey's `Grey-Webhook-Signature` is an HMAC over
/// the body carried in a *header*, which those places do not record — this
/// installation's own tracing redacts credential-bearing headers (see
/// [`crate::web::telemetry`]) — and it additionally proves the body was not
/// rewritten on the way. The two therefore defend against different exposures
/// rather than being two locks on one door, and the signature is what survives a
/// leaked URL.
#[derive(Clone, Serialize, Deserialize)]
pub struct GreyWebhookConfig {
    /// What to call this workflow, so it can be told apart from the others in a
    /// list of them.
    pub name: String,

    /// The shared secret set on the webhook in Grey's own configuration, used to
    /// verify the `Grey-Webhook-Signature` HMAC. Deliveries are refused while
    /// this is unset — see [`GreyWebhook::handle`] for why.
    #[serde(default)]
    pub secret: String,

    /// Optional base URL of the Grey status page, used to link the Todoist task back to Grey.
    #[serde(default)]
    pub dashboard_url: Option<String>,

    /// The amount of time to wait for a monitor to settle before surfacing the alert.
    #[serde(
        default = "default_alert_delay",
        with = "crate::serde_duration::minutes"
    )]
    pub alert_delay: chrono::Duration,

    /// The amount of time to wait after a monitor recovers before confirming the recovery.
    #[serde(
        default = "default_recovery_delay",
        with = "crate::serde_duration::minutes"
    )]
    pub recovery_delay: chrono::Duration,

    /// The minimum amount of impact required for a monitor's failure to stay in Todoist for later review.
    #[serde(
        default = "default_noise_duration",
        with = "crate::serde_duration::minutes"
    )]
    pub noise_duration: chrono::Duration,

    /// Filter applied to incoming events. The same fields Grey exposes to its own webhook filters
    /// are available here (`event`, `entity.*`, `state.*`).
    #[serde(default)]
    pub filter: crate::filter::Filter,

    #[serde(default = "default_todoist_config")]
    pub todoist: crate::publishers::TodoistTarget,
}

impl Display for GreyWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "grey/{}", self.name)
    }
}

impl Default for GreyWebhookConfig {
    /// Written out rather than derived, because a derived `Default` would leave
    /// every duration at zero — serde's `default = "…"` fallbacks only apply
    /// when deserializing, so the two paths would otherwise disagree about how
    /// long a monitor is given to settle.
    fn default() -> Self {
        Self {
            name: String::new(),
            secret: String::new(),
            dashboard_url: None,
            alert_delay: default_alert_delay(),
            recovery_delay: default_recovery_delay(),
            noise_duration: default_noise_duration(),
            filter: crate::filter::Filter::default(),
            todoist: default_todoist_config(),
        }
    }
}

fn default_alert_delay() -> chrono::Duration {
    chrono::Duration::minutes(5)
}

fn default_recovery_delay() -> chrono::Duration {
    chrono::Duration::hours(1)
}

fn default_noise_duration() -> chrono::Duration {
    chrono::Duration::minutes(5)
}

fn default_todoist_config() -> crate::publishers::TodoistTarget {
    crate::publishers::TodoistTarget {
        project: Some("Life".into()),
        section: Some("Tasks & Chores".into()),
        ..Default::default()
    }
}

#[derive(Clone)]
pub struct GreyWebhook;

impl GreyWebhook {
    /// Parses Grey's `t=<unix-seconds>,v1=<hex>` signature header into its timestamp and raw bytes.
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
                            "The timestamp in the Grey-Webhook-Signature header is invalid.",
                            &[
                                "Ensure that you are only sending Grey webhooks to this endpoint.",
                                "Check that the webhook is configured correctly in your Grey configuration.",
                            ],
                        )
                    })?;

                let signature = hex::decode(signature).or_user_err(&[
                    "The signature in the Grey-Webhook-Signature header is not valid hex.",
                    "Ensure that you are only sending Grey webhooks to this endpoint.",
                    "Check that the webhook is configured correctly in your Grey configuration.",
                ])?;

                Ok((timestamp, signature))
            }
            _ => Err(human_errors::user(
                "The Grey-Webhook-Signature header did not contain a valid signature.",
                &[
                    "Ensure that you are only sending Grey webhooks to this endpoint.",
                    "Check that the webhook is configured correctly in your Grey configuration.",
                ],
            )),
        }
    }

    /// Verifies the Grey webhook signature.
    ///
    /// Grey signs webhooks using the scheme [documented for Tailscale](https://tailscale.com/kb/1213/webhooks):
    /// HMAC-SHA256 over `"<timestamp>.<body>"`, with the signature carried in the
    /// `Grey-Webhook-Signature` header as `t=<timestamp>,v1=<hex_signature>`.
    ///
    /// The `now` parameter is the point in time against which the signature timestamp is validated.
    /// This should be the time at which the request was originally received (rather than the current
    /// time) so that retries of a previously received request continue to validate successfully.
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
                    "The Grey webhook signature timestamp is too old or too far in the future (got {timestamp})"
                ),
                &[
                    "Ensure that the system clock on this server is accurate.",
                    "Check that the webhook is configured correctly in your Grey configuration.",
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
                "Ensure that the webhook secret on this workflow matches the one set on the webhook in your Grey configuration.",
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

Turns the state changes reported by
[Grey](https://github.com/SierraSoftworks/grey) into a single Todoist task per
monitor that tells a coherent story: raised when the monitor goes unhealthy,
updated when it recovers, and closed out afterwards if the incident turned out
not to matter. One monitor is one task, so a flapping probe does not produce a
task per flap.

What it deliberately does *not* do is tell you immediately. Monitors blip, and
an alert for every blip is an alert nobody reads, so the three waiting periods
below decide what actually reaches you.

## Getting the address

Save the workflow first. Its address is generated when it is created and shown
on the workflow afterwards; there is nothing to paste into Grey until then.

Then add a webhook to Grey's own configuration pointing at that address, and
restart or reload Grey so it picks the change up. Grey's `docs/guide/webhooks.md`
in [its repository](https://github.com/SierraSoftworks/grey) describes the
configuration block and the payload it sends.

**Status page** is optional and purely cosmetic: when set, each task links back
to it so you can see the wider picture without hunting for the address.

## The webhook secret

The `secret` on that same webhook block in Grey's configuration is what Grey
signs each delivery with, sending an HMAC of the body in the
`Grey-Webhook-Signature` header. Generate a long random string, put it there,
and paste the same value into **Webhook secret** here. They have to match
exactly: Grey computes the signature with its copy and we check it with ours.

This is not a second lock on the same door as the address. The address travels
in the URL, so it is written into reverse-proxy access logs, into Grey's own
delivery records, and into anything sitting between the two — it is the part of
the request most likely to end up somewhere it should not be. The secret never
appears in the URL, only in a header, so a leaked address does not leak it. The
signature also covers the body, which the address cannot: it proves nothing
rewrote the state change on the way.

**A workflow with no secret refuses every delivery.** An empty field is not
treated as "skip the check" — that would leave a workflow silently
unauthenticated at exactly the moment somebody forgot to finish setting it up,
so a missing secret fails closed instead. It also means the check cannot be
turned off by clearing the box.

## The three waiting periods

These are what turn a stream of state changes into something worth reading.

**Wait before alerting** (default 5 minutes) is how long a monitor has to stay
unhealthy before you hear about it. The task is scheduled that far ahead rather
than created immediately, so a monitor that recovers inside the window has its
pending alert quietly withdrawn and never becomes a task at all.

**Wait before confirming recovery** (default 60 minutes) is how long a monitor
has to stay healthy before the incident is treated as over. A relapse inside
this window is recognised as the same incident and re-escalated immediately,
rather than starting the alert delay again — an operator is already watching
it, so making them wait five more minutes helps nobody.

**Keep incidents longer than** (default 5 minutes) decides what happens once a
monitor recovers. Incidents shorter than this are completed for you; anything
longer stays in Todoist for you to review, with the total impact time recorded
on it. Grey debounces recovery internally for five minutes, and that five
minutes is discounted before this comparison, so the duration you are setting a
threshold against is real impact rather than Grey's settling window.

## Grey's own nodes

A clustered Grey also reports on itself. Its nodes decide a probe's health by
quorum, so one node with a bad uplink cannot raise an alert alone; instead Grey
sends a `node.state_changed` event (`entity.type == "node"`, named by the node
identifier) when a node is **degraded** (a quorum of its probes fail from its
vantage point while the cluster reads them passing) or **silent** (it has
stopped recording samples). These are handled like any other monitor: one task
per node, listing the probes it disagrees on, raised and recovered through the
same waiting periods. Every node in the cluster reports every node, so the
delivery still arrives when the affected node is the one that cannot send it;
repeated deliveries of the same transition collapse onto the one task.

## Choosing which monitors to act on

The filter runs against each state change and can match on `event`,
`entity.type` (`probe`, `cron` or `node`), `entity.name`, `state.current`,
`state.previous`, `state.healthy`, `state.was_healthy` and
`state.availability`. A monitor's own tags are available as `tags.<name>` (or
`entity.tags.<name>`), which is usually the most useful of the lot:

```
entity.type == "probe" && tags.environment == "production"
```

Node events carry no tags, so a filter on tags alone excludes them; add
`|| entity.type == "node"` to keep them, or use `entity.type != "node"` to route
them to a separate workflow.

Leave it empty to act on every change Grey reports. Be careful narrowing it
after the fact: a filter that admits the unhealthy event but rejects the
recovery leaves a task open with nothing to close it.
"#;

crate::register_job!(GreyWebhook);
crate::register_workflow_type!(GreyWebhook);

impl crate::workflows::ConfigurableWorkflow for GreyWebhook {
    type ConfigType = GreyWebhookConfig;

    fn type_id() -> &'static str {
        "grey"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Grey".to_string(),
            description: "Raises a task when one of your Grey monitors goes unhealthy, and closes the story out when it recovers."
                .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Webhook {
                source: "grey".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Production monitors".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, so you can tell it apart from your others.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: secret),
                    "Webhook secret",
                    FieldKind::Secret {
                        placeholder: Some("a long random string".into()),
                        generator: true,
                        generator_bytes: 32,
                    },
                )
                .with_help(
                    "The `secret` set on this webhook in Grey's own configuration. Grey signs the body of every state change with it, which is what proves the delivery came from your Grey and was not rewritten on the way — the address alone cannot say either, and it travels in the URL where logs can see it. It must be the same value on both sides, and state changes are refused while this is empty.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: dashboard_url),
                    "Status page",
                    FieldKind::Url {
                        placeholder: Some("https://grey.example.com/".into()),
                    },
                )
                .with_help(
                    "Optional. When set, each task links back here so you can see the wider picture without hunting for the address.",
                ),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: alert_delay),
                    "Wait before alerting (minutes)",
                    FieldKind::Number {
                        min: Some(0.0),
                        max: None,
                        step: Some(1.0),
                    },
                )
                .with_help(
                    "How long a monitor has to stay unhealthy before you hear about it. A blip that clears inside this window never becomes a task.",
                )
                .with_default(5),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: recovery_delay),
                    "Wait before confirming recovery (minutes)",
                    FieldKind::Number {
                        min: Some(0.0),
                        max: None,
                        step: Some(1.0),
                    },
                )
                .with_help(
                    "How long a monitor has to stay healthy before its task is tidied away. A relapse inside this window is treated as the same incident.",
                )
                .with_default(60),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: noise_duration),
                    "Keep incidents longer than (minutes)",
                    FieldKind::Number {
                        min: Some(0.0),
                        max: None,
                        step: Some(1.0),
                    },
                )
                .with_help(
                    "Incidents shorter than this are closed for you once they recover; anything longer stays in Todoist for you to review.",
                )
                .with_default(5),
                FieldDescriptor::new(
                    crate::config_path!(GreyWebhookConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec![
                            "event".into(),
                            "entity.type".into(),
                            "entity.name".into(),
                            "state.current".into(),
                            "state.previous".into(),
                            "state.healthy".into(),
                            "state.was_healthy".into(),
                            "state.availability".into(),
                        ],
                    },
                )
                .with_help(
                    "Only act on the state changes matching this, such as entity.type == \"probe\" (or \"cron\" / \"node\" for Grey's own nodes). A monitor's own tags are available as tags.<name>. Leave it empty to act on every change.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                GreyWebhookConfig,
                project = Some("Life"),
                section = Some("Tasks & Chores")
            ))
            .collect(),
        }
    }
}

impl Job for GreyWebhook {
    type JobType = crate::webhooks::WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/grey"
    }

    #[instrument("webhooks.grey.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the delivery, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<GreyWebhookConfig>(services).await? else {
            return Ok(());
        };

        let event = &job.event;

        // Everything below this point happens *before* the payload is parsed, so
        // that a delivery we cannot attribute to Grey is never interpreted, let
        // alone acted on.
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
                "Received a Grey webhook for a workflow with no secret configured; rejecting request."
            );
            return Ok(());
        }

        let Some(signature) = Self::header(event, "grey-webhook-signature") else {
            warn!(
                "Received a Grey webhook without a Grey-Webhook-Signature header; rejecting request."
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
                "Failed to verify Grey webhook signature, rejecting request: {}",
                err
            );
            return Ok(());
        }

        let event: GreyWebhookEvent = event.json()?;

        if !config.filter.matches(&event)? {
            info!(
                "Grey event for {} '{}' did not match filter; ignoring.",
                event.entity.entity_type, event.entity.name
            );
            return Ok(());
        }

        let todoist_config = config.todoist.clone();
        let dashboard_url = config.dashboard_url.clone();

        let now = event.timestamp;
        let unique_key = event.unique_key();

        let debouncer = Debouncer::new(
            services.kv(),
            GREY_FAILURES_PARTITION,
            DebounceConfig {
                window: config.recovery_delay,
            },
        );

        let state = if event.state.healthy {
            debouncer.on_recovered(&unique_key, now).await?
        } else {
            Some(debouncer.on_triggered(&unique_key, now).await?)
        };

        match state {
            None => {
                // We recovered without a record of a failure, so there's nothing to do.
                info!(
                    "Grey {} '{}' sent a recovery event without a prior failure record, ignoring.",
                    event.entity.entity_type, event.entity.name
                );
            }
            Some(Detection::Triggered { first_triggered_at }) => {
                info!(
                    "Grey {} '{}' sent an alert.",
                    event.entity.entity_type, event.entity.name,
                );

                services
                    .queue()
                    .purge(TodoistCompleteTask::partition(), unique_key.clone())
                    .await?;

                TodoistUpsertTask::dispatch_delayed(
                    TodoistUpsertTaskPayload {
                        unique_key: unique_key.clone(),
                        title: event.task_title(dashboard_url.as_deref()),
                        description: Some(event.task_description()),
                        due: TodoistDueDate::DateTime(first_triggered_at),
                        priority: Some(event.priority()),
                        config: todoist_config,
                        ..Default::default()
                    },
                    Some(unique_key.clone().into()),
                    ((first_triggered_at + config.alert_delay) - now).max(chrono::Duration::zero()),
                    services,
                )
                .await?;
            }
            Some(Detection::Recovering { triggered_for }) => {
                // If the monitor returned to healthy before the alert debounce window elapsed, the
                // operator's task never surfaced — its delayed upsert is still sitting in the queue —
                // so cancel that pending alert and don't surface anything. A brief blip that clears
                // within the alert window must never reach Todoist (previously these produced spurious
                // "recovered after 0s" tasks). The debounce state is deliberately left in place so a
                // relapse within the recovery window is still recognised as the same flapping incident
                // and re-escalated immediately.
                if triggered_for < config.alert_delay {
                    info!(
                        "Grey {} '{}' recovered ({} -> {}) after only {}, before the {} alert window elapsed; purging the pending alert without surfacing a task.",
                        event.entity.entity_type,
                        event.entity.name,
                        event.state.previous,
                        event.state.current,
                        format_duration(triggered_for),
                        format_duration(config.alert_delay),
                    );

                    services
                        .queue()
                        .purge(TodoistUpsertTask::partition(), unique_key.clone())
                        .await?;

                    return Ok(());
                }

                info!(
                    "Grey {} '{}' recovered ({} -> {}); triggered for {}, confirming in {}.",
                    event.entity.entity_type,
                    event.entity.name,
                    event.state.previous,
                    event.state.current,
                    format_duration(triggered_for),
                    format_duration(config.recovery_delay),
                );

                // Grey internally has a 5m settling window before it sends a resolved event, so let's adjust for that
                let true_impact_duration = triggered_for - chrono::Duration::minutes(5);

                TodoistUpsertTask::dispatch(
                    TodoistUpsertTaskPayload {
                        unique_key: unique_key.clone(),
                        title: event
                            .recovered_title(dashboard_url.as_deref(), true_impact_duration),
                        description: Some(event.recovered_description(true_impact_duration)),
                        due: TodoistDueDate::DateTime(now),
                        priority: Some(2),
                        config: todoist_config.clone(),
                        ..Default::default()
                    },
                    Some(unique_key.clone().into()),
                    services,
                )
                .await?;

                if true_impact_duration < config.noise_duration {
                    TodoistCompleteTask::dispatch_delayed(
                        TodoistCompleteTaskPayload {
                            unique_key: unique_key.clone(),
                            config: todoist_config,
                        },
                        Some(unique_key.into()),
                        config.recovery_delay,
                        services,
                    )
                    .await?;
                }
            }
        }

        Ok(())
    }
}

/// A Grey `probe.state_changed` / `cron.state_changed` / `node.state_changed` webhook payload.
///
/// This mirrors the wire shape of `grey_api::WebhookEvent` (see Grey's `docs/guide/webhooks.md`),
/// carrying only the fields we read. The full `probe`/`cron`/`node` snapshots are kept as raw JSON
/// so we can surface a little extra context without coupling to Grey's internal types.
#[allow(dead_code)]
#[derive(Deserialize)]
struct GreyWebhookEvent {
    #[serde(default)]
    version: String,
    id: String,
    event: String,
    timestamp: DateTime<Utc>,
    entity: GreyEntity,
    state: GreyState,
    #[serde(default)]
    probe: Option<serde_json::Value>,
    #[serde(default)]
    cron: Option<serde_json::Value>,
    #[serde(default)]
    node: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct GreyEntity {
    #[serde(rename = "type")]
    entity_type: String,
    name: String,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct GreyState {
    current: String,
    previous: String,
    healthy: bool,
    was_healthy: bool,
    #[serde(default)]
    since: Option<DateTime<Utc>>,
    #[serde(default)]
    availability: Option<f64>,
}

impl GreyWebhookEvent {
    /// A stable per-monitor key (`grey/<type>/<name>`) used to correlate the Todoist task, the
    /// [`GREY_FAILURES_PARTITION`] state record, and the queue idempotency key for the task's
    /// active-state upserts.
    fn unique_key(&self) -> String {
        format!("grey/{}/{}", self.entity.entity_type, self.entity.name)
    }

    /// A human-friendly label for the entity type (`Probe` / `Cron`), title-cased for display.
    fn entity_label(&self) -> String {
        let mut chars = self.entity.entity_type.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => "Monitor".to_string(),
        }
    }

    /// Builds a `**Grey**: <Entity> `<name>` <status>` title, linking back to the Grey status page
    /// when one is configured. The `status` clause describes the monitor's current situation, e.g.
    /// `is failing`, `is recovering`, or `has recovered after 12m`.
    fn title_with_status(&self, dashboard_url: Option<&str>, status: &str) -> String {
        let body = format!("{} `{}` {}", self.entity_label(), self.entity.name, status);

        match dashboard_url {
            Some(url) if !url.is_empty() => format!("[**Grey**]({url}): {body}"),
            _ => format!("**Grey**: {body}"),
        }
    }

    /// The Todoist task title for the monitor's current (unhealthy) state.
    fn task_title(&self, dashboard_url: Option<&str>) -> String {
        self.title_with_status(dashboard_url, &format!("is {}", self.state.current))
    }

    /// The title stamped onto the task once a monitor has stayed healthy for the full recovery
    /// window, carrying the total impact time.
    fn recovered_title(&self, dashboard_url: Option<&str>, impact: chrono::Duration) -> String {
        self.title_with_status(
            dashboard_url,
            &format!("recovered after {}", format_duration(impact)),
        )
    }

    /// The `- **Since:** … / - **Availability:** … / - **Tags:** …` context lines shared by every
    /// task description. Tags are sorted so the rendered description (and thus the upsert hash) is
    /// deterministic.
    fn detail_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();

        if let Some(since) = self.state.since {
            lines.push(format!("- **Since:** {}", since.to_rfc3339()));
        }

        if let Some(availability) = self.state.availability {
            lines.push(format!("- **Availability:** {availability:.2}%"));
        }

        if !self.entity.tags.is_empty() {
            let mut tags: Vec<_> = self.entity.tags.iter().collect();
            tags.sort();
            let rendered = tags
                .into_iter()
                .map(|(key, value)| format!("`{key}={value}`"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("- **Tags:** {rendered}"));
        }

        lines
    }

    /// The `_Event `…` (schema …)._` footer shared by every task description.
    fn event_footer(&self) -> String {
        format!("_Event `{}` (schema {})._", self.id, self.version)
    }

    /// A markdown description giving an operator the context needed to triage the alert: the
    /// transition, when it happened, availability, tags, and the most recent failure detail.
    fn task_description(&self) -> String {
        let mut lines = vec![
            format!(
                "**{} `{}`** changed from **{}** to **{}**.",
                self.entity_label(),
                self.entity.name,
                self.state.previous,
                self.state.current
            ),
            String::new(),
        ];

        lines.extend(self.detail_lines());

        if let Some(detail) = self.failure_detail() {
            lines.push(String::new());
            lines.push(format!("**Latest detail:** {detail}"));
        }

        lines.push(String::new());
        lines.push(self.event_footer());

        lines.join("\n")
    }

    /// The description stamped onto the task once a monitor has fully recovered, recording the total
    /// impact time of the incident.
    fn recovered_description(&self, impact: chrono::Duration) -> String {
        let mut lines = vec![
            format!(
                "**{} `{}`** has **recovered**.",
                self.entity_label(),
                self.entity.name
            ),
            String::new(),
            format!("- **Total impact time:** {}", format_duration(impact)),
        ];

        lines.extend(self.detail_lines());

        lines.push(String::new());
        lines.push(self.event_footer());

        lines.join("\n")
    }

    /// The most recent failure detail from the embedded snapshot: the latest probe history bucket's
    /// message, a cron's last check-in, or the probes a node disagrees with its cluster on. Returns
    /// `None` when nothing useful is available.
    fn failure_detail(&self) -> Option<String> {
        if let Some(node) = &self.node {
            return Self::node_detail(node);
        }

        if let Some(probe) = &self.probe {
            let message = probe
                .get("history")?
                .as_array()?
                .last()?
                .get("message")?
                .as_str()?
                .trim();

            if !message.is_empty() {
                return Some(message.to_string());
            }
        }

        if let Some(cron) = &self.cron {
            let checkin = cron.get("last_checkin")?;
            let status = checkin.get("status").and_then(|s| s.as_str());
            let message = checkin
                .get("message")
                .and_then(|m| m.as_str())
                .map(str::trim)
                .filter(|m| !m.is_empty());

            return match (status, message) {
                (Some(status), Some(message)) => {
                    Some(format!("last check-in `{status}`: {message}"))
                }
                (Some(status), None) => Some(format!("last check-in `{status}`")),
                (None, Some(message)) => Some(message.to_string()),
                (None, None) => None,
            };
        }

        None
    }

    /// Summarises a `node.state_changed` snapshot: how many of the node's probes disagree with the
    /// cluster (a degraded node), or when it was last heard from (a silent one), naming the
    /// disagreeing probes so the operator knows what that node is seeing.
    fn node_detail(node: &serde_json::Value) -> Option<String> {
        let count = |key: &str| node.get(key).and_then(|v| v.as_u64());
        let mut disagreeing: Vec<&str> = node
            .get("probes")
            .and_then(|p| p.as_object())
            .map(|probes| {
                probes
                    .iter()
                    .filter(|(_, view)| {
                        view.get("failing").and_then(|v| v.as_bool()) == Some(true)
                            && view.get("cluster_failing").and_then(|v| v.as_bool()) == Some(false)
                    })
                    .map(|(name, _)| name.as_str())
                    .collect()
            })
            .unwrap_or_default();
        disagreeing.sort_unstable();

        match node.get("status").and_then(|s| s.as_str()) {
            Some("silent") => {
                let last = node.get("last_updated").and_then(|v| v.as_str());
                Some(match last {
                    Some(last) => format!("no samples recorded since {last}"),
                    None => "no samples recorded".to_string(),
                })
            }
            _ => {
                let (count, total) = (count("disagreeing")?, count("total")?);
                let mut detail =
                    format!("{count} of {total} probes fail from this node but not from the cluster");
                if !disagreeing.is_empty() {
                    let names = disagreeing
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    detail.push_str(&format!(": {names}"));
                }
                Some(detail)
            }
        }
    }

    /// The Todoist priority for an unhealthy monitor, escalating the most disruptive states.
    fn priority(&self) -> i32 {
        match self.state.current.as_str() {
            // Probe down, cron failed, or a run that never started are the most urgent.
            "failing" | "failed" | "missing" => 4,
            // An overrunning ("stuck") run is concerning but the job is at least alive, and a Grey
            // node that disagrees with its cluster (or has gone quiet) affects monitoring coverage
            // rather than a monitored service.
            "stuck" | "degraded" | "silent" => 3,
            _ => 3,
        }
    }
}

impl Filterable for GreyWebhookEvent {
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        use crate::filter::FilterValue;

        match key {
            "event" => self.event.as_str().into(),
            "entity.type" | "entity.kind" => self.entity.entity_type.as_str().into(),
            "entity.name" => self.entity.name.as_str().into(),
            "state.current" => self.state.current.as_str().into(),
            "state.previous" => self.state.previous.as_str().into(),
            "state.healthy" => FilterValue::Bool(self.state.healthy),
            "state.was_healthy" => FilterValue::Bool(self.state.was_healthy),
            "state.availability" => self
                .state
                .availability
                .map(FilterValue::Number)
                .unwrap_or(FilterValue::Null),
            k if k.starts_with("entity.tags.") => self
                .entity
                .tags
                .get(&k["entity.tags.".len()..])
                .map(|v| v.as_str().into())
                .unwrap_or(FilterValue::Null),
            k if k.starts_with("tags.") => self
                .entity
                .tags
                .get(&k["tags.".len()..])
                .map(|v| v.as_str().into())
                .unwrap_or(FilterValue::Null),
            _ => FilterValue::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::db::PeekedMessage;
    use crate::publishers::TodoistUpsertTaskState;
    use crate::services::debounce::DebounceState;
    use crate::webhooks::{WebhookDelivery, WebhookEvent};
    use crate::workflow_store::{WorkflowDraft, WorkflowStore};

    use super::*;

    /// Builds a `probe.state_changed` body with explicit event and `since` timestamps, so tests can
    /// drive the debounce state machine deterministically.
    fn probe_event_at(name: &str, healthy: bool, timestamp: &str, since: &str) -> String {
        let (current, previous) = if healthy {
            ("passing", "failing")
        } else {
            ("failing", "passing")
        };

        format!(
            r#"{{
                "version": "v1",
                "id": "evt-1",
                "event": "probe.state_changed",
                "timestamp": "{timestamp}",
                "entity": {{ "type": "probe", "name": "{name}", "tags": {{ "service": "Web" }} }},
                "state": {{
                    "current": "{current}",
                    "previous": "{previous}",
                    "healthy": {healthy},
                    "was_healthy": {was_healthy},
                    "since": "{since}",
                    "availability": 98.7
                }},
                "probe": {{ "history": [{{ "pass": false, "message": "HTTP 503" }}] }}
            }}"#,
            was_healthy = !healthy
        )
    }

    /// A `probe.state_changed` body with the canonical fixed timestamps used by the parsing tests.
    fn probe_event(name: &str, healthy: bool) -> String {
        probe_event_at(
            name,
            healthy,
            "2026-06-19T12:00:00Z",
            "2026-06-19T11:59:30Z",
        )
    }

    /// The timings a real deployment uses, stated explicitly so the debounce
    /// behaviour is exercised rather than collapsing onto whatever the defaults
    /// happen to be, together with the secret every delivery below is signed
    /// with.
    fn config() -> serde_json::Value {
        serde_json::json!({
            "name": "Production monitors",
            "secret": SECRET,
            "alert_delay": 5,
            "recovery_delay": 60,
            "noise_duration": 5,
        })
    }

    /// The secret these tests pretend was set on both this workflow and the
    /// webhook block in Grey's own configuration.
    const SECRET: &str = "a-long-random-string";

    /// Signs a body the way Grey does: `t=<unix-seconds>,v1=<hex>` over
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

    async fn mock_services() -> crate::services::ServicesContainer<crate::db::TenantDb> {
        crate::services::ServicesContainer::new_mock()
            .await
            .unwrap()
    }

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "grey".into(),
                config,
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id
    }

    /// A delivery carrying the signature Grey itself would have sent for it,
    /// dated now so it falls inside the freshness window [`run`] checks against.
    fn delivery(workflow: automate_api::WorkflowId, body: String) -> WebhookDelivery {
        let signature = sign(SECRET, Utc::now().timestamp(), &body);
        delivery_with(workflow, body, &[("Grey-Webhook-Signature", &signature)])
    }

    /// A delivery carrying whatever headers the test wants, for the ones about
    /// what happens when the signature is wrong, absent or unmatched.
    fn delivery_with(
        workflow: automate_api::WorkflowId,
        body: String,
        headers: &[(&str, &str)],
    ) -> WebhookDelivery {
        WebhookDelivery {
            workflow,
            event: WebhookEvent {
                body,
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
        GreyWebhook
            .handle(
                JobContext::new(services.clone(), Utc::now(), None, None),
                delivery,
            )
            .await
    }

    /// Parses an RFC 3339 timestamp into a UTC instant. The explicit return type pins the otherwise
    /// ambiguous `FromStr` impl (chrono has one per timezone) so call sites stay terse.
    fn dt(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    /// Peeks every pending Todoist upsert enqueued by the handler.
    async fn peek_upserts<S: Services>(
        services: &S,
    ) -> Vec<PeekedMessage<TodoistUpsertTaskPayload>> {
        services
            .queue()
            .peek(TodoistUpsertTask::partition(), 16)
            .await
            .unwrap()
    }

    /// Peeks every pending Todoist cleanup (complete-task) enqueued by the handler.
    async fn peek_completes<S: Services>(
        services: &S,
    ) -> Vec<PeekedMessage<TodoistCompleteTaskPayload>> {
        services
            .queue()
            .peek(TodoistCompleteTask::partition(), 16)
            .await
            .unwrap()
    }

    /// Fetches the persisted debounce state for a monitor, if any.
    async fn failure_record<S: Services>(services: &S, unique_key: &str) -> Option<DebounceState> {
        services
            .kv()
            .get::<DebounceState>(GREY_FAILURES_PARTITION, unique_key.to_string())
            .await
            .unwrap()
    }

    /// Records an existing surfaced Todoist task so the recovery path treats the monitor as alerted.
    async fn seed_task<S: Services>(services: &S, unique_key: &str) {
        services
            .kv()
            .set(
                "todoist/task",
                unique_key.to_string(),
                TodoistUpsertTaskState {
                    id: "task-123".to_string(),
                    hash: "seed".to_string(),
                    title: Some("seed".to_string()),
                },
            )
            .await
            .unwrap();
    }

    #[test]
    fn test_unique_key_is_stable_per_monitor() {
        let failing: GreyWebhookEvent =
            serde_json::from_str(&probe_event("web.prod", false)).unwrap();
        let recovered: GreyWebhookEvent =
            serde_json::from_str(&probe_event("web.prod", true)).unwrap();

        // The key identifies the monitor, not its current state, so the failing and recovered
        // events for one probe correlate to the same Todoist task.
        assert_eq!(failing.unique_key(), "grey/probe/web.prod");
        assert_eq!(failing.unique_key(), recovered.unique_key());
    }

    #[test]
    fn test_task_title_links_when_dashboard_configured() {
        let event: GreyWebhookEvent =
            serde_json::from_str(&probe_event("web.prod", false)).unwrap();

        assert_eq!(
            event.task_title(Some("https://grey.example.com")),
            "[**Grey**](https://grey.example.com): Probe `web.prod` is failing"
        );
        assert_eq!(
            event.task_title(None),
            "**Grey**: Probe `web.prod` is failing"
        );
    }

    #[test]
    fn test_task_description_includes_failure_detail_and_tags() {
        let event: GreyWebhookEvent =
            serde_json::from_str(&probe_event("web.prod", false)).unwrap();
        let description = event.task_description();

        assert!(description.contains("changed from **passing** to **failing**"));
        assert!(description.contains("- **Availability:** 98.70%"));
        assert!(description.contains("`service=Web`"));
        assert!(description.contains("**Latest detail:** HTTP 503"));
    }

    #[test]
    fn test_cron_failure_detail_uses_last_checkin() {
        let body = r#"{
            "id": "evt-2",
            "event": "cron.state_changed",
            "timestamp": "2026-06-19T12:00:00Z",
            "entity": { "type": "cron", "name": "backup", "tags": {} },
            "state": { "current": "failed", "previous": "succeeded", "healthy": false, "was_healthy": true },
            "cron": { "last_checkin": { "status": "failed", "message": "exit code 1" } }
        }"#;
        let event: GreyWebhookEvent = serde_json::from_str(body).unwrap();

        assert_eq!(event.entity_label(), "Cron");
        assert_eq!(event.priority(), 4);
        assert_eq!(
            event.failure_detail().as_deref(),
            Some("last check-in `failed`: exit code 1")
        );
    }

    fn node_event(status: &str) -> String {
        let (healthy, previous) = match status {
            "healthy" => ("true", "degraded"),
            _ => ("false", "healthy"),
        };
        format!(
            r#"{{
                "version": "v1",
                "id": "evt-3",
                "event": "node.state_changed",
                "timestamp": "2026-06-19T12:00:00Z",
                "entity": {{ "type": "node", "name": "1p3x9k", "tags": {{}} }},
                "state": {{ "current": "{status}", "previous": "{previous}", "healthy": {healthy}, "was_healthy": {was_healthy}, "since": "2026-06-19T11:00:00Z" }},
                "node": {{
                    "id": "1p3x9k",
                    "status": "{status}",
                    "last_updated": "2026-06-19T10:30:00Z",
                    "probes": {{
                        "web.prod": {{ "failing": true, "cluster_failing": false }},
                        "api.prod": {{ "failing": true, "cluster_failing": false }},
                        "db.prod": {{ "failing": true, "cluster_failing": true }}
                    }},
                    "disagreeing": 2,
                    "total": 3,
                    "quorum": 2
                }}
            }}"#,
            was_healthy = healthy == "false"
        )
    }

    #[test]
    fn test_node_events_parse_and_describe_the_disagreement() {
        let event: GreyWebhookEvent = serde_json::from_str(&node_event("degraded")).unwrap();

        assert_eq!(event.unique_key(), "grey/node/1p3x9k");
        assert_eq!(event.entity_label(), "Node");
        assert_eq!(event.priority(), 3);
        assert_eq!(
            event.failure_detail().as_deref(),
            Some("2 of 3 probes fail from this node but not from the cluster: `api.prod`, `web.prod`")
        );

        let description = event.task_description();
        assert!(description.contains("**Node `1p3x9k`** changed from **healthy** to **degraded**"), "{description}");
        assert!(description.contains("**Latest detail:** 2 of 3 probes"), "{description}");
        assert!(!description.contains("**Tags:**"), "node events carry no tags: {description}");
        assert_eq!(event.task_title(None), "**Grey**: Node `1p3x9k` is degraded");

        let silent: GreyWebhookEvent = serde_json::from_str(&node_event("silent")).unwrap();
        assert_eq!(
            silent.failure_detail().as_deref(),
            Some("no samples recorded since 2026-06-19T10:30:00Z")
        );

        let recovered: GreyWebhookEvent = serde_json::from_str(&node_event("healthy")).unwrap();
        assert!(recovered.state.healthy);
        assert!(
            Filter::new(r#"entity.type == "node""#).unwrap().matches(&recovered).unwrap()
        );
    }

    #[test]
    fn test_filter_exposes_grey_fields() {
        let event: GreyWebhookEvent =
            serde_json::from_str(&probe_event("web.prod", false)).unwrap();

        assert!(
            Filter::new(r#"entity.type == "probe""#)
                .unwrap()
                .matches(&event)
                .unwrap()
        );
        assert!(
            Filter::new("state.healthy == false")
                .unwrap()
                .matches(&event)
                .unwrap()
        );
        assert!(
            Filter::new(r#"tags.service == "Web""#)
                .unwrap()
                .matches(&event)
                .unwrap()
        );
        assert!(
            !Filter::new(r#"entity.type == "cron""#)
                .unwrap()
                .matches(&event)
                .unwrap()
        );
    }

    #[test]
    fn test_format_duration_is_compact() {
        use chrono::Duration;

        assert_eq!(format_duration(Duration::seconds(45)), "45s");
        assert_eq!(format_duration(Duration::minutes(12)), "12m");
        assert_eq!(format_duration(Duration::minutes(65)), "1h 5m");
        assert_eq!(format_duration(Duration::hours(2)), "2h");
        assert_eq!(format_duration(Duration::seconds(0)), "0s");
        // Negative spans are clamped, and sub-minute precision is dropped past an hour.
        assert_eq!(format_duration(Duration::seconds(-30)), "0s");
        assert_eq!(format_duration(Duration::seconds(3661)), "1h 1m");
    }

    #[test]
    fn test_recovered_titles() {
        let event: GreyWebhookEvent = serde_json::from_str(&probe_event("web.prod", true)).unwrap();

        assert_eq!(
            event.recovered_title(
                Some("https://grey.example.com"),
                chrono::Duration::minutes(15)
            ),
            "[**Grey**](https://grey.example.com): Probe `web.prod` recovered after 15m"
        );
    }

    #[test]
    fn test_recovered_description_reports_impact() {
        let event: GreyWebhookEvent = serde_json::from_str(&probe_event("web.prod", true)).unwrap();

        let description = event.recovered_description(chrono::Duration::minutes(15));
        assert!(description.contains("has **recovered**"));
        assert!(description.contains("- **Total impact time:** 15m"));
    }

    #[test]
    fn a_waiting_period_is_written_down_as_a_number_of_minutes() {
        // The form collects minutes, so a stored configuration has to hold them
        // as a plain number rather than as chrono's (seconds, nanos) pair —
        // otherwise nothing anybody could type into the field would load.
        let config: GreyWebhookConfig = serde_json::from_value(serde_json::json!({
            "name": "Production monitors",
            "alert_delay": 15,
        }))
        .expect("a configuration in minutes should load");

        assert_eq!(config.alert_delay, chrono::Duration::minutes(15));
        assert_eq!(
            config.recovery_delay,
            chrono::Duration::hours(1),
            "an omitted waiting period should fall back to its default rather than to zero",
        );

        let round_tripped = serde_json::to_value(&config).unwrap();
        assert_eq!(round_tripped["alert_delay"], 15);
    }

    #[test]
    fn a_negative_waiting_period_is_refused_by_name() {
        // "Wait minus five minutes" is not a thing we could do, and silently
        // treating it as zero would alert instantly on a monitor somebody
        // thought they had told us to be patient about.
        let Err(err) = serde_json::from_value::<GreyWebhookConfig>(serde_json::json!({
            "name": "Production monitors",
            "alert_delay": -5,
        })) else {
            panic!("a negative waiting period should not load");
        };

        assert!(format!("{err}").contains("negative"), "{err}");
    }

    #[test]
    fn a_stored_configuration_names_the_workflow_it_describes() {
        let workflow = crate::workflows::lookup("grey").expect("the type is registered");

        assert_eq!(
            workflow
                .describe(&serde_json::json!({ "name": "Production monitors" }))
                .unwrap(),
            "Production monitors",
        );
    }

    #[tokio::test]
    async fn test_unhealthy_schedules_delayed_alert() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", false)),
        )
        .await
        .expect("unhealthy event should be handled");

        // No task is created immediately; a single delayed upsert is scheduled ~5 minutes out.
        let upserts = peek_upserts(&services).await;
        assert_eq!(
            upserts.len(),
            1,
            "exactly one delayed alert should be queued"
        );

        let alert = &upserts[0];
        assert_eq!(alert.key, unique_key);
        assert_eq!(alert.payload.priority, Some(4));
        assert!(alert.payload.title.contains("is failing"));
        assert!(
            alert.hidden_until > Utc::now() + chrono::Duration::minutes(4),
            "the alert should stay hidden for roughly the alert delay"
        );
        assert!(alert.hidden_until < Utc::now() + chrono::Duration::minutes(6));

        // A brand-new failure never schedules a cleanup; there is nothing to tidy up yet.
        assert!(
            peek_completes(&services).await.is_empty(),
            "no cleanup should be queued for a fresh failure"
        );

        // The first- and last-failure times are recorded, and we are not (yet) recovering.
        let record = failure_record(&services, unique_key)
            .await
            .expect("a failure record should be written");
        assert_eq!(record.first_triggered_at, dt("2026-06-19T12:00:00Z"));
        assert_eq!(record.last_triggered_at, dt("2026-06-19T12:00:00Z"));
        assert!(record.recovering_since.is_none());
    }

    #[tokio::test]
    async fn test_recovery_without_prior_failure_is_ignored() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // A healthy event with no recorded incident is a no-op: nothing to update, nothing to clean.
        run(
            &services,
            &delivery(workflow, probe_event("web.prod", true)),
        )
        .await
        .unwrap();

        assert!(
            peek_upserts(&services).await.is_empty(),
            "no task should be touched without a prior failure"
        );
        assert!(
            peek_completes(&services).await.is_empty(),
            "no cleanup should be queued without a prior failure"
        );
        assert!(
            failure_record(&services, unique_key).await.is_none(),
            "no debounce state should be created for a stray recovery"
        );
    }

    #[tokio::test]
    async fn test_recovery_updates_task_and_schedules_cleanup_when_noise() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // The alert already surfaced (a task exists) and a delayed alert is still queued from when
        // the incident first fired. The incident first went unhealthy at 11:53, so by the 12:00
        // recovery it was triggered for 7m — a true impact of ~2m once Grey's 5m settling window is
        // discounted, which is below the 5m noise threshold.
        seed_task(&services, unique_key).await;
        services
            .queue()
            .enqueue(
                TodoistUpsertTask::partition(),
                TodoistUpsertTaskPayload {
                    unique_key: unique_key.to_string(),
                    ..Default::default()
                },
                Some(unique_key.to_string().into()),
                Some(chrono::Duration::minutes(5)),
            )
            .await
            .unwrap();
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                DebounceState {
                    first_triggered_at: dt("2026-06-19T11:53:00Z"),
                    last_triggered_at: dt("2026-06-19T11:55:00Z"),
                    recovering_since: None,
                },
            )
            .await
            .unwrap();

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", true)),
        )
        .await
        .unwrap();

        // The pending alert is replaced in place by an immediate recovered update carrying the true
        // impact time (~2m).
        let upserts = peek_upserts(&services).await;
        assert_eq!(
            upserts.len(),
            1,
            "the recovered update should replace the pending alert on the same key"
        );
        let recovered = &upserts[0];
        assert_eq!(recovered.key, unique_key);
        assert_eq!(recovered.payload.priority, Some(2));
        assert!(
            recovered.payload.title.contains("recovered after 2m"),
            "the recovered update should carry the true impact time, got {:?}",
            recovered.payload.title
        );
        assert!(
            recovered.hidden_until <= Utc::now(),
            "the recovered update should fire immediately"
        );

        // Because it was just noise, a cleanup is deferred by the recovery window (~1h) to remove the
        // task once we are confident it was not a flap.
        let completes = peek_completes(&services).await;
        assert_eq!(completes.len(), 1, "a delayed cleanup should be queued");
        assert_eq!(completes[0].key, unique_key);
        assert!(
            completes[0].hidden_until > Utc::now() + chrono::Duration::minutes(55),
            "the cleanup should be deferred by roughly the recovery window"
        );

        // We now track that we are recovering as of the healthy event, retaining the first-failure
        // time so a later relapse still measures impact from the original outage.
        let record = failure_record(&services, unique_key)
            .await
            .expect("a recovery record");
        assert_eq!(record.recovering_since, Some(dt("2026-06-19T12:00:00Z")));
        assert_eq!(record.first_triggered_at, dt("2026-06-19T11:53:00Z"));
    }

    #[tokio::test]
    async fn test_recovery_updates_task_without_cleanup_when_impactful() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // The incident first went unhealthy at 11:45, so by the 12:00 recovery it was triggered for
        // 15m — a true impact of 10m once Grey's 5m settling window is discounted, which exceeds the
        // 5m noise threshold and therefore stays in Todoist for review.
        seed_task(&services, unique_key).await;
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                DebounceState {
                    first_triggered_at: dt("2026-06-19T11:45:00Z"),
                    last_triggered_at: dt("2026-06-19T11:50:00Z"),
                    recovering_since: None,
                },
            )
            .await
            .unwrap();

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", true)),
        )
        .await
        .unwrap();

        // An immediate recovered update carries the true impact time (10m)...
        let upserts = peek_upserts(&services).await;
        assert_eq!(upserts.len(), 1, "one recovered update should be queued");
        let recovered = &upserts[0];
        assert_eq!(recovered.key, unique_key);
        assert_eq!(recovered.payload.priority, Some(2));
        assert!(
            recovered.payload.title.contains("recovered after 10m"),
            "the recovered update should carry the true impact time, got {:?}",
            recovered.payload.title
        );
        assert!(
            recovered.hidden_until <= Utc::now(),
            "the recovered update should fire immediately"
        );

        // ...but because the impact was above the noise threshold, no cleanup is scheduled: the task
        // stays for the operator to review.
        assert!(
            peek_completes(&services).await.is_empty(),
            "an impactful incident should not schedule a cleanup"
        );

        let record = failure_record(&services, unique_key)
            .await
            .expect("a recovery record");
        assert_eq!(record.recovering_since, Some(dt("2026-06-19T12:00:00Z")));
        assert_eq!(record.first_triggered_at, dt("2026-06-19T11:45:00Z"));
    }

    #[tokio::test]
    async fn test_recovery_before_alert_surfaces_purges_pending_alert() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // The monitor first went unhealthy at 11:58 and its alert is still sitting in the queue
        // (it only fires at 12:03, once the 5m alert window elapses). The 12:00 recovery therefore
        // lands inside the alert window — only 2m of impact — before the operator was ever notified.
        services
            .queue()
            .enqueue(
                TodoistUpsertTask::partition(),
                TodoistUpsertTaskPayload {
                    unique_key: unique_key.to_string(),
                    ..Default::default()
                },
                Some(unique_key.to_string().into()),
                Some(chrono::Duration::minutes(3)),
            )
            .await
            .unwrap();
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                DebounceState {
                    first_triggered_at: dt("2026-06-19T11:58:00Z"),
                    last_triggered_at: dt("2026-06-19T11:58:00Z"),
                    recovering_since: None,
                },
            )
            .await
            .unwrap();

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", true)),
        )
        .await
        .unwrap();

        // The pending alert is purged and nothing is surfaced: a blip that clears before the alert
        // window elapses never reaches Todoist (previously this produced a "recovered after 0s" task).
        assert!(
            peek_upserts(&services).await.is_empty(),
            "the pending alert should be purged and no recovered task surfaced"
        );
        assert!(
            peek_completes(&services).await.is_empty(),
            "no cleanup should be queued for an unsurfaced blip"
        );

        // Crucially the debounce state is retained (now marked recovering, first-failure time
        // preserved) so we still know when the probe last failed and can treat a relapse as flapping.
        let record = failure_record(&services, unique_key)
            .await
            .expect("the debounce state should be retained when the alert never surfaced");
        assert_eq!(record.first_triggered_at, dt("2026-06-19T11:58:00Z"));
        assert_eq!(record.recovering_since, Some(dt("2026-06-19T12:00:00Z")));

        // A relapse within the recovery window is therefore recognised as the same incident and
        // re-escalated immediately (dated to the original 11:58 first failure), not debounced afresh.
        // (12:05 is past the original 12:03 alert deadline, so the escalation fires immediately.)
        run(
            &services,
            &delivery(
                workflow,
                probe_event_at(
                    "web.prod",
                    false,
                    "2026-06-19T12:05:00Z",
                    "2026-06-19T12:05:00Z",
                ),
            ),
        )
        .await
        .unwrap();

        let upserts = peek_upserts(&services).await;
        let escalation = upserts
            .iter()
            .find(|m| m.key == unique_key)
            .expect("the relapse should re-escalate the alert");
        assert!(
            escalation.hidden_until <= Utc::now(),
            "a relapse of an ongoing incident should re-escalate immediately"
        );
        assert!(escalation.payload.title.contains("is failing"));
    }

    #[tokio::test]
    async fn test_refailure_after_settling_reescalates_and_cancels_cleanup() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // The monitor first failed at 11:30, last reported unhealthy at 11:40, and is currently
        // recovering (since 11:45), with a pending noise cleanup queued to remove the task.
        seed_task(&services, unique_key).await;
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                DebounceState {
                    first_triggered_at: dt("2026-06-19T11:30:00Z"),
                    last_triggered_at: dt("2026-06-19T11:40:00Z"),
                    recovering_since: Some(dt("2026-06-19T11:45:00Z")),
                },
            )
            .await
            .unwrap();
        services
            .queue()
            .enqueue(
                TodoistCompleteTask::partition(),
                TodoistCompleteTaskPayload {
                    unique_key: unique_key.to_string(),
                    ..Default::default()
                },
                Some(unique_key.to_string().into()),
                Some(chrono::Duration::minutes(60)),
            )
            .await
            .unwrap();

        // A failure at 12:00 — 20 minutes after the last unhealthy report, so inside the recovery
        // window — and well beyond the original 5m settling time, so it re-escalates immediately.
        run(
            &services,
            &delivery(workflow, probe_event("web.prod", false)),
        )
        .await
        .unwrap();

        // The pending cleanup is cancelled so the task cannot be removed while the monitor is down...
        assert!(
            peek_completes(&services).await.is_empty(),
            "the pending cleanup should be purged on re-failure"
        );
        // ...and the task is re-escalated to unhealthy immediately.
        let upserts = peek_upserts(&services).await;
        let escalation = upserts
            .iter()
            .find(|m| m.key == unique_key)
            .expect("an immediate re-escalation");
        assert_eq!(escalation.payload.priority, Some(4));
        assert!(escalation.payload.title.contains("is failing"));
        assert!(
            escalation.hidden_until <= Utc::now(),
            "the re-escalation should fire immediately"
        );

        // The first-failure time is preserved (the relapse is part of the same incident), the last
        // failure advances to now, and the recovery state is cleared.
        let record = failure_record(&services, unique_key)
            .await
            .expect("an updated record");
        assert_eq!(record.first_triggered_at, dt("2026-06-19T11:30:00Z"));
        assert_eq!(record.last_triggered_at, dt("2026-06-19T12:00:00Z"));
        assert!(
            record.recovering_since.is_none(),
            "recovery state should be cleared on re-failure"
        );
    }

    #[tokio::test]
    async fn test_new_incident_failure_purges_stale_cleanup() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;
        let unique_key = "grey/probe/web.prod";

        // A previous incident recovered at 10:45 (last failure 10:30) and left a pending noise
        // cleanup queued.
        seed_task(&services, unique_key).await;
        services
            .kv()
            .set(
                GREY_FAILURES_PARTITION,
                unique_key.to_string(),
                DebounceState {
                    first_triggered_at: dt("2026-06-19T10:00:00Z"),
                    last_triggered_at: dt("2026-06-19T10:30:00Z"),
                    recovering_since: Some(dt("2026-06-19T10:45:00Z")),
                },
            )
            .await
            .unwrap();
        services
            .queue()
            .enqueue(
                TodoistCompleteTask::partition(),
                TodoistCompleteTaskPayload {
                    unique_key: unique_key.to_string(),
                    ..Default::default()
                },
                Some(unique_key.to_string().into()),
                Some(chrono::Duration::minutes(60)),
            )
            .await
            .unwrap();

        // A failure at 12:00 — 90 minutes after the last failure, so a brand-new incident, not a
        // relapse — must still cancel the stale cleanup so it can never remove the task while the
        // monitor is unhealthy.
        run(
            &services,
            &delivery(workflow, probe_event("web.prod", false)),
        )
        .await
        .unwrap();

        assert!(
            peek_completes(&services).await.is_empty(),
            "the stale cleanup should be purged even for a new incident"
        );
        // It is a new incident: the alert is delayed (not an immediate escalation) and the
        // first-failure time is reset.
        let upserts = peek_upserts(&services).await;
        let alert = upserts
            .iter()
            .find(|m| m.key == unique_key)
            .expect("a delayed alert");
        assert!(
            alert.hidden_until > Utc::now() + chrono::Duration::minutes(4),
            "a new incident should be debounced, not escalated immediately"
        );
        assert_eq!(
            failure_record(&services, unique_key)
                .await
                .unwrap()
                .first_triggered_at,
            dt("2026-06-19T12:00:00Z")
        );
    }

    #[tokio::test]
    async fn an_event_the_filter_rejects_is_ignored() {
        // The filter is the only thing between a busy tailnet of monitors and a
        // task for every one of them, so a state change it does not match has to
        // leave no trace at all — not even debounce state.
        let services = mock_services().await;
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Crons only",
                "secret": SECRET,
                "filter": r#"entity.type == "cron""#,
            }),
        )
        .await;

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", false)),
        )
        .await
        .unwrap();

        assert!(peek_upserts(&services).await.is_empty());
        assert!(
            failure_record(&services, "grey/probe/web.prod")
                .await
                .is_none(),
        );
    }

    #[tokio::test]
    async fn test_grey_webhook_invalid_json() {
        let services = mock_services().await;
        let workflow = store(&services, config()).await;

        let result = run(
            &services,
            &delivery(workflow, r#"{"invalid json"#.to_string()),
        )
        .await;

        assert!(result.is_err(), "Webhook should reject invalid JSON");
    }

    #[test]
    fn a_state_change_signed_with_the_configured_secret_is_accepted() {
        let body = probe_event("web.prod", false);
        let now = Utc::now();

        GreyWebhook::verify_signature(SECRET, &body, &sign(SECRET, now.timestamp(), &body), now)
            .expect("a signature Grey itself would have produced should verify");
    }

    #[test]
    fn a_state_change_signed_with_a_different_secret_is_refused() {
        // Knowing the URL is not knowing the secret, which is the whole point of
        // checking one.
        let body = probe_event("web.prod", false);
        let now = Utc::now();
        let signature = sign("somebody-elses-secret", now.timestamp(), &body);

        assert!(GreyWebhook::verify_signature(SECRET, &body, &signature, now).is_err());
    }

    #[test]
    fn a_body_altered_after_signing_no_longer_matches_its_signature() {
        // The signature covers the body, so replaying a genuine signature over a
        // payload somebody rewrote in transit has to fail — otherwise a leaked
        // URL would let anybody claim any monitor had fallen over.
        let now = Utc::now();
        let signature = sign(SECRET, now.timestamp(), &probe_event("web.prod", false));
        let tampered = probe_event("web.staging", false);

        assert!(GreyWebhook::verify_signature(SECRET, &tampered, &signature, now).is_err());
    }

    #[test]
    fn a_signature_header_in_some_other_shape_is_refused() {
        // Grey sends `t=…,v1=…`; anything else is not a signature we failed to
        // match, it is a header we cannot even read.
        let body = probe_event("web.prod", false);

        assert!(
            GreyWebhook::verify_signature(SECRET, &body, "not-a-signature", Utc::now()).is_err()
        );
        assert!(
            GreyWebhook::verify_signature(SECRET, &body, "t=1663781880", Utc::now()).is_err(),
            "a header with a timestamp but no digest proves nothing",
        );
    }

    #[test]
    fn a_signature_is_checked_against_when_the_delivery_arrived_not_when_it_is_retried() {
        // A delivery that failed and was requeued is verified again hours later.
        // Checking its timestamp against the current time would reject it for
        // being stale, so a retry that would have succeeded first time round
        // would fail forever.
        let received_at = Utc::now() - chrono::Duration::hours(6);
        let body = probe_event("web.prod", false);
        let signature = sign(SECRET, received_at.timestamp(), &body);

        assert!(
            GreyWebhook::verify_signature(SECRET, &body, &signature, Utc::now()).is_err(),
            "the freshness window should reject a six-hour-old signature against the current time",
        );
        GreyWebhook::verify_signature(SECRET, &body, &signature, received_at)
            .expect("the same signature should verify against the time it was received");
    }

    #[tokio::test]
    async fn a_state_change_signed_with_the_wrong_secret_raises_nothing() {
        // Each workflow carries its own secret, so a state change signed for
        // somebody else's must not be acted on by this one.
        let services = mock_services().await;
        let workflow = store(&services, config()).await;

        let body = probe_event("web.prod", false);
        let signature = sign("somebody-elses-secret", Utc::now().timestamp(), &body);
        let job = delivery_with(workflow, body, &[("Grey-Webhook-Signature", &signature)]);

        run(&services, &job)
            .await
            .expect("a mis-signed state change should be refused without erroring");

        assert!(peek_upserts(&services).await.is_empty());
        assert!(
            failure_record(&services, "grey/probe/web.prod")
                .await
                .is_none(),
            "an unverified delivery should not even have been interpreted",
        );
    }

    #[tokio::test]
    async fn a_state_change_whose_body_was_altered_after_signing_raises_nothing() {
        // The signature is what makes the payload trustworthy, so a body that
        // was rewritten between Grey signing it and us receiving it must never
        // reach the parser.
        let services = mock_services().await;
        let workflow = store(&services, config()).await;

        let signature = sign(
            SECRET,
            Utc::now().timestamp(),
            &probe_event("web.prod", false),
        );
        let job = delivery_with(
            workflow,
            probe_event("web.staging", false),
            &[("Grey-Webhook-Signature", &signature)],
        );

        run(&services, &job)
            .await
            .expect("a tampered state change should be refused without erroring");

        assert!(peek_upserts(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_state_change_with_no_signature_header_at_all_raises_nothing() {
        // Anybody can post to a URL, and the URL is the part of the request most
        // likely to have leaked. Without the header there is nothing to check,
        // and "nothing to check" is not the same as "checks out".
        let services = mock_services().await;
        let workflow = store(&services, config()).await;

        let job = delivery_with(workflow, probe_event("web.prod", false), &[]);

        run(&services, &job)
            .await
            .expect("an unsigned state change should be refused without erroring");

        assert!(peek_upserts(&services).await.is_empty());
    }

    #[tokio::test]
    async fn state_changes_are_refused_while_the_workflow_has_no_secret_configured() {
        // A half-finished workflow should raise nothing rather than raise
        // whatever anybody who found the URL cares to post. An empty field means
        // "cannot be verified", not "need not be verified".
        let services = mock_services().await;
        let workflow = store(
            &services,
            serde_json::json!({ "name": "Production monitors", "secret": "" }),
        )
        .await;

        run(
            &services,
            &delivery(workflow, probe_event("web.prod", false)),
        )
        .await
        .expect("an unverifiable state change should be refused without erroring");

        assert!(peek_upserts(&services).await.is_empty());
    }

    #[tokio::test]
    async fn the_signature_header_is_recognised_whatever_case_it_arrives_in() {
        // HTTP header names are case-insensitive and whatever proxy sits in
        // front of us is free to renormalise them, so a lowercase header must
        // not read as a missing one.
        let services = mock_services().await;
        let workflow = store(&services, config()).await;

        let body = probe_event("web.prod", false);
        let signature = sign(SECRET, Utc::now().timestamp(), &body);
        let job = delivery_with(workflow, body, &[("grey-webhook-signature", &signature)]);

        run(&services, &job)
            .await
            .expect("a correctly signed state change should be processed");

        assert_eq!(peek_upserts(&services).await.len(), 1);
    }
}
