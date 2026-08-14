//! Webhook handler for [Miniflux](https://miniflux.app/docs/webhooks.html).
//!
//! Miniflux tells us about an entry the moment it reads one, which is what the
//! polling [`crate::jobs::RssWorkflow`] cannot do: that one asks each feed on a
//! schedule, keeps its own watermark, and is as late as the schedule is coarse.
//! One aggregator that already does the fetching, the de-duplicating and the
//! remembering leaves this with only the part that is ours — deciding which
//! entries are worth a task, and writing them the way the RSS workflow writes
//! them, so that moving a feed across does not change what lands in Todoist.

use std::fmt::Display;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{
    prelude::*,
    publishers::{TodoistCreateTask, TodoistCreateTaskPayload, TodoistDueDate, TodoistTarget},
};

type HmacSha256 = Hmac<Sha256>;

/// What one person asked us to do with the entries their Miniflux reports.
///
/// This carries the secret Miniflux generated, because the address on its own
/// does not do the job people assumed it did. The address travels in the URL:
/// it is written to reverse-proxy access logs and to anything sitting between
/// Miniflux and here. It is the part of the request most likely to end up
/// somewhere it should not be. Miniflux's `X-Miniflux-Signature` is an HMAC
/// over the body carried in a *header*, which those places do not record — this
/// installation's own tracing redacts credential-bearing headers (see
/// [`crate::web::telemetry`]) — and it additionally proves the body was not
/// rewritten on the way.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct MinifluxWebhookConfig {
    /// What to call this workflow, so it can be told apart from the others in a
    /// list of them.
    pub name: String,

    /// The secret Miniflux shows under Settings → Integrations → Webhook, used
    /// to verify the `X-Miniflux-Signature` HMAC. Deliveries are refused while
    /// this is unset — see [`MinifluxWebhook::handle`] for why.
    #[serde(default)]
    pub secret: String,

    /// What to do with the entries a feed refresh discovered.
    #[serde(default)]
    pub new_entries: MinifluxEventConfig,

    /// What to do with the entries you starred in Miniflux yourself.
    #[serde(default)]
    pub saved_entries: MinifluxEventConfig,

    #[serde(default = "default_todoist_config")]
    pub todoist: TodoistTarget,
}

/// Whether one of Miniflux's two event types is filed, and which of its entries.
///
/// The two are configured separately because they mean different things: a feed
/// refresh is a firehose you narrow, whereas starring an entry is already a
/// person saying they want it. A single filter across both would have to be
/// written to suit whichever was noisier.
#[derive(Clone, Serialize, Deserialize)]
pub struct MinifluxEventConfig {
    pub enabled: bool,

    #[serde(default)]
    pub filter: Filter,
}

impl Default for MinifluxEventConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            filter: Filter::default(),
        }
    }
}

impl Display for MinifluxWebhookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "miniflux/{}", self.name)
    }
}

fn default_todoist_config() -> TodoistTarget {
    TodoistTarget {
        project: Some("Hobbies".into()),
        section: Some("Reading".into()),
        ..Default::default()
    }
}

#[derive(Clone)]
pub struct MinifluxWebhook;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Files a Todoist task for each entry your Miniflux reports. The task's title
links to the entry and is labelled with the feed it came from; its body is the
entry's content converted from HTML to Markdown, so most of the time you can
decide whether to read something without leaving your task list. That is the
same shape of task the built-in RSS workflow files, so moving a feed from there
to here does not change what your task list looks like.

Miniflux sends two kinds of event, and each is configured separately here:

- **New entries** — sent when a feed refresh discovers entries. This is a
  firehose, so it is the one worth filtering.
- **Saved entries** — sent when you save an entry in Miniflux yourself, which
  is already you saying you want it. Turn this on to make Miniflux's save
  button a "read this later" button.

Either can be turned off entirely, since Miniflux sends both to the same
address and does not let you choose per endpoint.

## Getting the address

Save the workflow first. Its address is generated when it is created and shown
on the workflow afterwards; there is nothing to paste into Miniflux until then.

Then, in Miniflux, open **Settings → Integrations → Webhook**, tick
**Activate webhook**, and set the webhook URL to this workflow's address.

## The webhook secret

Miniflux shows an auto-generated secret on that same page. Copy it into
**Webhook secret** here. Miniflux signs the body of every delivery with it and
sends the digest in `X-Miniflux-Signature`, which we check against our copy —
so the two have to match. Regenerating it in Miniflux means pasting the new
value here as well.

**A workflow with no secret refuses every delivery.** An empty field is not
treated as "skip the check" — that would leave a workflow silently
unauthenticated at exactly the moment somebody forgot to finish setting it up,
so a missing secret fails closed instead.

## Choosing which entries to file

Each filter runs against every entry in a delivery, and an entry is only filed
if it matches. They can match on `title`, `description`, `link`, `feed` and
`author`, all lower-cased before comparison, and on `tags`, which is the list
of categories Miniflux holds against the entry.

```
feed contains "security" || title contains "release"
```

Leave a filter empty to file every entry that event carries.

## No retries

Miniflux does not retry a delivery that fails, so an entry missed while this
service was down is missed for good. Saving it in Miniflux afterwards is the
way to bring it back.
"#;

impl MinifluxWebhook {
    /// Verifies the `X-Miniflux-Signature` header, which Miniflux populates
    /// with the hex-encoded HMAC-SHA256 of the raw request body, keyed with the
    /// secret shown under Settings → Integrations → Webhook.
    ///
    /// See https://miniflux.app/docs/webhooks.html#validating-signatures-from-miniflux
    fn verify_signature(
        secret: &str,
        body: &str,
        signature_header: &str,
    ) -> Result<(), human_errors::Error> {
        let expected_signature = hex::decode(signature_header.trim()).or_user_err(&[
            "The signature in the X-Miniflux-Signature header is not valid hex.",
            "Ensure that you are only sending Miniflux webhooks to this endpoint.",
        ])?;

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).wrap_user_err(
            "Failed to create HMAC instance with the provided secret.",
            &["Ensure that you have set a valid webhook secret on this workflow."],
        )?;

        mac.update(body.as_bytes());

        // `verify_slice` compares in constant time, so a wrong signature cannot
        // be walked one byte at a time by timing the rejections.
        mac.verify_slice(&expected_signature).wrap_user_err(
            "Webhook signature verification failed (signatures did not match).".to_string(),
            &[
                "Ensure that the webhook secret on this workflow matches the one shown under Settings → Integrations → Webhook in Miniflux.",
            ],
        )?;

        Ok(())
    }

    /// Files one entry, if the event's filter wants it.
    async fn file(
        config: &MinifluxWebhookConfig,
        event: &MinifluxEventConfig,
        entry: MinifluxEntryRef<'_>,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<(), human_errors::Error> {
        if !event.filter.matches(&entry)? {
            info!(
                "Miniflux entry '{}' did not match filter; ignoring.",
                entry.entry.title
            );
            return Ok(());
        }

        // Only used to turn the relative links inside an entry's content into
        // ones that can be clicked from Todoist.
        let base_url = entry
            .entry
            .url
            .parse::<reqwest::Url>()
            .ok()
            .or_else(|| entry.feed().and_then(|feed| feed.site_url.parse().ok()));

        TodoistCreateTask::dispatch(
            TodoistCreateTaskPayload {
                title: format!(
                    "[{}]({}): {}",
                    entry.label(&config.name),
                    entry.entry.url,
                    entry.entry.title,
                ),
                description: base_url
                    .filter(|_| !entry.entry.content.is_empty())
                    .map(|base_url| {
                        crate::parsers::html_to_markdown(
                            &html_escape::decode_html_entities(&entry.entry.content),
                            base_url,
                        )
                    }),
                due: TodoistDueDate::Today,
                config: config.todoist.clone(),
                ..Default::default()
            },
            None,
            services,
        )
        .await
    }
}

crate::register_job!(MinifluxWebhook);
crate::register_workflow_type!(MinifluxWebhook);

impl crate::workflows::ConfigurableWorkflow for MinifluxWebhook {
    type ConfigType = MinifluxWebhookConfig;

    fn type_id() -> &'static str {
        "miniflux"
    }

    fn describe(config: &Self::ConfigType) -> String {
        config.name.clone()
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor};

        WorkflowTypeDescriptor {
            id: Self::type_id().to_string(),
            name: "Miniflux".to_string(),
            description: "Files a task for each entry your Miniflux reads or you save."
                .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Webhook {
                source: "miniflux".to_string(),
            },
            fields: [
                FieldDescriptor::new(
                    crate::config_path!(MinifluxWebhookConfig: name),
                    "Name",
                    FieldKind::Text {
                        placeholder: Some("Reading".into()),
                    },
                )
                .with_help(
                    "Used to label this workflow, and any entry whose feed did not name itself.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(MinifluxWebhookConfig: secret),
                    "Webhook secret",
                    // No generator: Miniflux issues this one, so anything we
                    // made up here could only ever be the wrong value.
                    FieldKind::Secret {
                        placeholder: Some("c4fdaf87900b72777cbe8e97d17b97a8…".into()),
                        generator: false,
                        generator_bytes: 32,
                    },
                )
                .with_help(
                    "The secret Miniflux shows under Settings → Integrations → Webhook. Miniflux signs the body of every delivery with it, which is what proves the delivery came from your Miniflux and was not rewritten on the way — the address alone cannot say either, and it travels in the URL where logs can see it. Entries are refused while this is empty.",
                )
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(MinifluxWebhookConfig: new_entries.enabled),
                    "File new entries",
                    FieldKind::Boolean,
                )
                .with_help("File a task for each entry a feed refresh discovers.")
                .with_default(true),
                FieldDescriptor::new(
                    crate::config_path!(MinifluxWebhookConfig: new_entries.filter),
                    "File these new entries",
                    FieldKind::Filter {
                        fields: Self::filter_fields(),
                    },
                )
                .with_help(
                    "Only file the newly discovered entries matching this. Leave it empty to file every one of them.",
                ),
                FieldDescriptor::new(
                    crate::config_path!(MinifluxWebhookConfig: saved_entries.enabled),
                    "File saved entries",
                    FieldKind::Boolean,
                )
                .with_help(
                    "File a task when you save an entry in Miniflux, turning its save button into a read-it-later button.",
                )
                .with_default(true),
                FieldDescriptor::new(
                    crate::config_path!(MinifluxWebhookConfig: saved_entries.filter),
                    "File these saved entries",
                    FieldKind::Filter {
                        fields: Self::filter_fields(),
                    },
                )
                .with_help(
                    "Only file the saved entries matching this. Leave it empty to file every entry you save.",
                ),
            ]
            .into_iter()
            .chain(crate::todoist_target_fields!(
                MinifluxWebhookConfig,
                project = Some("Hobbies"),
                section = Some("Reading")
            ))
            .collect(),
        }
    }
}

impl MinifluxWebhook {
    /// The names an entry answers to, shared by both filters so that one cannot
    /// quietly offer something the other does not.
    fn filter_fields() -> Vec<String> {
        ["title", "description", "link", "feed", "author", "tags"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }
}

impl Job for MinifluxWebhook {
    type JobType = crate::webhooks::WebhookDelivery;

    fn partition() -> &'static str {
        "webhooks/miniflux"
    }

    #[instrument("webhooks.miniflux.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // Read now rather than carried in the delivery, so that an edit made
        // between the delivery arriving and this running is the one that applies.
        let Some(config) = job.config::<MinifluxWebhookConfig>(services).await? else {
            return Ok(());
        };

        let event = &job.event;

        // Everything below this point happens *before* the payload is parsed, so
        // that a delivery we cannot attribute to Miniflux is never interpreted,
        // let alone acted on.
        //
        // A rejection returns `Ok(())` rather than an error: nothing about a bad
        // signature improves by trying again, so raising here would only leave
        // the delivery retrying forever and hiding real failures behind it. The
        // log line is the record that it happened.

        // No secret configured means we refuse, rather than accept anything. The
        // alternative — treating an empty secret as "skip the check" — would make
        // a workflow silently unauthenticated exactly when somebody forgot to
        // finish setting it up, and a forgotten field should fail closed.
        if config.secret.is_empty() {
            warn!(
                "Received a Miniflux webhook for a workflow with no secret configured; rejecting request."
            );
            return Ok(());
        }

        let Some(signature) = event.header("x-miniflux-signature") else {
            warn!(
                "Received a Miniflux webhook without an X-Miniflux-Signature header; rejecting request."
            );
            return Ok(());
        };

        if let Err(err) = Self::verify_signature(&config.secret, &event.body, signature) {
            warn!(
                "Failed to verify Miniflux webhook signature, rejecting request: {}",
                err
            );
            return Ok(());
        }

        match event.json::<MinifluxEvent>()? {
            MinifluxEvent::NewEntries { feed, entries } => {
                if !config.new_entries.enabled {
                    debug!("This workflow does not file newly discovered entries; ignoring.");
                    return Ok(());
                }

                for entry in &entries {
                    Self::file(
                        &config,
                        &config.new_entries,
                        MinifluxEntryRef {
                            entry,
                            feed: Some(&feed),
                        },
                        services,
                    )
                    .await?;
                }
            }
            MinifluxEvent::SaveEntry { entry } => {
                if !config.saved_entries.enabled {
                    debug!("This workflow does not file saved entries; ignoring.");
                    return Ok(());
                }

                Self::file(
                    &config,
                    &config.saved_entries,
                    MinifluxEntryRef {
                        entry: &entry,
                        feed: None,
                    },
                    services,
                )
                .await?;
            }
            MinifluxEvent::Unknown => {
                info!("Ignoring a Miniflux event of a type we do not handle.");
            }
        }

        Ok(())
    }
}

/// The two documented event shapes, told apart by the `event_type` in the body
/// rather than the header that repeats it, since only the body is signed.
///
/// A version of Miniflux that grows a third one lands in [`MinifluxEvent::Unknown`]
/// rather than failing to parse, so an event we have never heard of is ignored
/// instead of being retried until it expires.
#[derive(Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
enum MinifluxEvent {
    NewEntries {
        feed: MinifluxFeed,
        #[serde(default)]
        entries: Vec<MinifluxEntry>,
    },
    SaveEntry {
        entry: MinifluxEntry,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct MinifluxFeed {
    #[serde(default)]
    title: String,
    #[serde(default)]
    site_url: String,
}

#[derive(Deserialize)]
struct MinifluxEntry {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    tags: Vec<String>,

    /// Present on a saved entry, which carries its feed inside itself, and
    /// absent on a discovered one, where the feed is named once for the batch.
    #[serde(default)]
    feed: Option<MinifluxFeed>,
}

/// An entry together with whichever of the two places its feed was named in.
struct MinifluxEntryRef<'a> {
    entry: &'a MinifluxEntry,
    feed: Option<&'a MinifluxFeed>,
}

impl<'a> MinifluxEntryRef<'a> {
    fn feed(&self) -> Option<&'a MinifluxFeed> {
        self.entry.feed.as_ref().or(self.feed)
    }

    /// What to call the source in the task's title. The feed's own name where
    /// it has one, since a workflow covers every feed in the account.
    fn label(&self, fallback: &'a str) -> &'a str {
        self.feed()
            .map(|feed| feed.title.as_str())
            .filter(|title| !title.is_empty())
            .unwrap_or(fallback)
    }
}

impl Filterable for MinifluxEntryRef<'_> {
    fn get(&self, key: &str) -> crate::filter::FilterValue<'_> {
        match key {
            "title" => self.entry.title.to_lowercase().into(),
            "description" => self.entry.content.to_lowercase().into(),
            "link" => self.entry.url.to_lowercase().into(),
            "feed" => self
                .feed()
                .map(|feed| feed.title.to_lowercase())
                .unwrap_or_default()
                .into(),
            "author" => self.entry.author.to_lowercase().into(),
            "tags" => crate::filter::FilterValue::Tuple(
                self.entry
                    .tags
                    .iter()
                    .map(|tag| tag.to_lowercase().into())
                    .collect(),
            ),
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

    /// A delivery as Miniflux sends one after a feed refresh.
    const NEW_ENTRIES: &str = r#"{"event_type":"new_entries","feed":{"id":8,"user_id":1,"feed_url":"https://example.org/feed.xml","site_url":"https://example.org","title":"Example website","checked_at":"2023-09-10T12:48:43.428196-07:00"},"entries":[{"id":231,"user_id":1,"feed_id":8,"status":"unread","hash":"1163a9","title":"Example","url":"https://example.org/article","comments_url":"","published_at":"2023-08-17T19:29:22Z","created_at":"2023-09-10T12:48:43.428196-07:00","changed_at":"2023-09-10T12:48:43.428196-07:00","content":"<p>Some HTML content with a <a href=\"/relative\">relative link</a></p>","author":"Alice","share_code":"","starred":false,"reading_time":1,"enclosures":[],"tags":["Some category"]}]}"#;

    /// A delivery as Miniflux sends one when somebody saves an entry, where the
    /// feed lives inside the entry rather than beside it.
    const SAVE_ENTRY: &str = r#"{"event_type":"save_entry","entry":{"id":592,"user_id":1,"feed_id":9,"status":"read","hash":"ed97d3","title":"Some example","url":"https://example.org/saved","comments_url":"","published_at":"2023-09-10T19:13:40Z","created_at":"2023-09-10T20:06:23.000332Z","changed_at":"2023-09-11T00:39:49.615812Z","content":"Saved HTML content","author":"","share_code":"","starred":true,"reading_time":1,"enclosures":[],"tags":[],"feed":{"id":9,"user_id":1,"feed_url":"https://example.org/feed.xml","site_url":"https://example.org","title":"Example website","checked_at":"2023-09-10T20:07:22.956279Z"}}}"#;

    /// The secret these tests pretend Miniflux generated.
    const SECRET: &str = "c4fdaf87900b72777cbe8e97d17b97a87d28ae078a462ef4ac5f2541fcf00ce6";

    /// Signs a body the way Miniflux does: the hex HMAC-SHA256 of the body.
    fn sign(secret: &str, body: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// A configuration that files everything Miniflux sends.
    fn config() -> serde_json::Value {
        serde_json::json!({ "name": "Reading", "secret": SECRET })
    }

    async fn store(
        services: &(impl Services + Send + Sync + 'static),
        config: serde_json::Value,
    ) -> automate_api::WorkflowId {
        WorkflowStore::new(services)
            .with_index(services)
            .create(WorkflowDraft {
                type_id: "miniflux".into(),
                config,
                schedule: None,
                enabled: true,
            })
            .await
            .expect("store the workflow")
            .id
    }

    /// A delivery carrying the signature Miniflux itself would have sent for it.
    fn delivery(workflow: automate_api::WorkflowId, body: &str) -> WebhookDelivery {
        let signature = sign(SECRET, body);
        delivery_with(workflow, body, &[("X-Miniflux-Signature", &signature)])
    }

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
        MinifluxWebhook
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
    async fn a_discovered_entry_is_filed_the_way_the_rss_workflow_files_one() {
        // The point of moving a feed from the RSS workflow to Miniflux is that
        // the aggregation changes and the task does not, so the title is still
        // "[source](link): title" and the body is still the entry as Markdown.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery(workflow, NEW_ENTRIES))
            .await
            .unwrap();

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0].payload["title"],
            "[Example website](https://example.org/article): Example",
        );

        let description = filed[0].payload["description"].as_str().unwrap();
        assert!(
            description.contains("Some HTML content"),
            "the entry's content should be quoted into the task, so it can be triaged without going to look",
        );
        assert!(
            description.contains("https://example.org/relative"),
            "relative links should be resolved against the entry, or they cannot be followed from Todoist: {description}",
        );
    }

    #[tokio::test]
    async fn a_saved_entry_is_filed_from_the_feed_it_carries_with_it() {
        // A save event names its feed inside the entry rather than beside it, so
        // reading only the outer shape would leave saved tasks unlabelled.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(&services, &delivery(workflow, SAVE_ENTRY))
            .await
            .unwrap();

        let filed = filed(&services).await;
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0].payload["title"],
            "[Example website](https://example.org/saved): Some example",
        );
    }

    #[tokio::test]
    async fn each_event_type_is_filtered_on_its_own() {
        // Saving an entry is already a person asking for it, so a filter written
        // to thin out a noisy refresh must not also throw away what they saved.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Reading",
                "secret": SECRET,
                "new_entries": { "enabled": true, "filter": r#"title contains "nothing""# },
                "saved_entries": { "enabled": true },
            }),
        )
        .await;

        run(&services, &delivery(workflow, NEW_ENTRIES))
            .await
            .unwrap();
        assert!(filed(&services).await.is_empty());

        run(&services, &delivery(workflow, SAVE_ENTRY))
            .await
            .unwrap();
        assert_eq!(filed(&services).await.len(), 1);
    }

    #[tokio::test]
    async fn an_event_type_that_is_turned_off_files_nothing() {
        // Miniflux posts both event types to the same address, so turning one
        // off here is the only way to not receive it.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Saved only",
                "secret": SECRET,
                "new_entries": { "enabled": false },
                "saved_entries": { "enabled": true },
            }),
        )
        .await;

        run(&services, &delivery(workflow, NEW_ENTRIES))
            .await
            .unwrap();

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn an_entry_can_be_selected_by_the_tags_miniflux_holds_against_it() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({
                "name": "Reading",
                "secret": SECRET,
                "new_entries": { "enabled": true, "filter": r#""some category" in tags"# },
            }),
        )
        .await;

        run(&services, &delivery(workflow, NEW_ENTRIES))
            .await
            .unwrap();

        assert_eq!(filed(&services).await.len(), 1);
    }

    #[tokio::test]
    async fn an_event_type_we_do_not_handle_is_ignored_rather_than_retried() {
        // Miniflux does not retry, but we do; a future event type that failed to
        // parse would sit in the queue failing until it expired.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        run(
            &services,
            &delivery(workflow, r#"{"event_type":"something_new"}"#),
        )
        .await
        .expect("an unrecognised event should be ignored without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn a_delivery_signed_with_the_wrong_secret_files_nothing() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let signature = sign("somebody-elses-secret", NEW_ENTRIES);
        let job = delivery_with(
            workflow,
            NEW_ENTRIES,
            &[("X-Miniflux-Signature", &signature)],
        );

        run(&services, &job)
            .await
            .expect("a mis-signed delivery should be refused without erroring");

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

        run(&services, &delivery_with(workflow, NEW_ENTRIES, &[]))
            .await
            .expect("an unsigned delivery should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn deliveries_are_refused_while_the_workflow_has_no_secret_configured() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(
            &services,
            serde_json::json!({ "name": "Reading", "secret": "" }),
        )
        .await;

        run(&services, &delivery(workflow, NEW_ENTRIES))
            .await
            .expect("an unverifiable delivery should be refused without erroring");

        assert!(filed(&services).await.is_empty());
    }

    #[tokio::test]
    async fn the_signature_header_is_recognised_whatever_case_it_arrives_in() {
        // HTTP header names are case-insensitive and whatever proxy sits in
        // front of us is free to renormalise them.
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let workflow = store(&services, config()).await;

        let signature = sign(SECRET, NEW_ENTRIES);
        let job = delivery_with(
            workflow,
            NEW_ENTRIES,
            &[("x-miniflux-signature", &signature)],
        );

        run(&services, &job)
            .await
            .expect("a correctly signed delivery should be processed");

        assert_eq!(filed(&services).await.len(), 1);
    }

    #[test]
    fn a_body_altered_after_signing_no_longer_matches_its_signature() {
        let signature = sign(SECRET, NEW_ENTRIES);
        let tampered = NEW_ENTRIES.replace("Example website", "Somewhere else");

        assert!(MinifluxWebhook::verify_signature(SECRET, &tampered, &signature).is_err());
        MinifluxWebhook::verify_signature(SECRET, NEW_ENTRIES, &signature)
            .expect("a signature Miniflux itself would have produced should verify");
    }

    #[test]
    fn a_stored_configuration_names_the_workflow_it_describes() {
        let workflow = crate::workflows::lookup("miniflux").expect("the type is registered");

        assert_eq!(
            workflow
                .describe(&serde_json::json!({ "name": "Reading" }))
                .unwrap(),
            "Reading",
        );
    }
}
