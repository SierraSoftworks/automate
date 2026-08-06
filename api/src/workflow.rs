//! The vocabulary a workflow type uses to describe the form that configures it.
//!
//! A workflow used to be a block of TOML, which meant the only thing that could
//! read it was serde and the only way to write one was to know the schema by
//! heart. Configuring one from a browser needs the server to say what a workflow
//! type wants: what to ask for, what to call it, and how to render the control
//! that collects it.
//!
//! The descriptor is deliberately a small closed set of field kinds rather than
//! JSON Schema. JSON Schema can describe far more than this application will
//! ever ask for, and none of the extra vocabulary would render — a renderer that
//! silently ignores most of the spec it claims to implement is worse than one
//! that implements a smaller thing completely. Every variant of [`FieldKind`]
//! maps onto exactly one control in the UI, so a descriptor that parses is a
//! descriptor that draws.
//!
//! Descriptors are served rather than compiled into the UI, so adding a workflow
//! type is a change to the agent alone. That is the point: the browser learns
//! what an RSS workflow needs by asking, not by being rebuilt.
//!
//! # Nested values
//!
//! Field names are dotted paths — `todoist.project` addresses `project` inside
//! the `todoist` object. The alternative, a recursive descriptor tree, would let
//! a type nest arbitrarily deep; in practice nothing nests more than one level
//! (a workflow names its target, and that is the whole of it), so the tree would
//! cost a recursive renderer to buy a generality nobody asked for. A path is
//! also the thing the renderer actually needs, since it has to write into a
//! `serde_json::Value` either way.

use serde::{Deserialize, Serialize};

use crate::{OptionItem, WorkflowId};

/// What causes a workflow to run.
///
/// This also decides where its configuration is stored, which is why it is
/// settled here rather than left to each call site. Configurations live in the
/// key-value store under the partition their trigger names, mirroring the queue
/// partition the trigger dispatches into, so that the schedule and the thing it
/// schedules are always found in the same place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowTrigger {
    /// Runs on a schedule the user provides.
    Cron {
        /// The schedule a new workflow of this type starts with, so that
        /// somebody adding a feed does not have to have an opinion about how
        /// often to poll it before they can save.
        default_schedule: String,
    },

    /// Runs when a delivery arrives on this workflow's webhook URL.
    Webhook {
        /// The provider whose payloads this accepts, e.g. `github`.
        source: String,
    },

    /// Runs when a provider's shared webhook receives an event which belongs to
    /// this workflow's selected connection.
    RoutedWebhook {
        /// The provider whose shared endpoint receives the payload.
        source: String,
    },
}

impl WorkflowTrigger {
    /// The storage partition that holds configurations for this trigger.
    pub fn partition(&self) -> String {
        match self {
            Self::Cron { .. } => "cron".to_string(),
            Self::Webhook { source } | Self::RoutedWebhook { source } => {
                format!("webhooks/{source}")
            }
        }
    }
}

/// The control used to collect one value, and everything the renderer needs to
/// draw it.
///
/// Each variant carries its own constraints rather than sharing a bag of
/// optional properties, so a `Number`'s bounds cannot be set on a checkbox and a
/// `Select` cannot be declared without options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldKind {
    /// A single line of free text.
    Text {
        /// Shown in the empty control as an example of what is wanted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },

    /// Several lines of free text.
    TextArea {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },

    /// A URL. Separate from [`FieldKind::Text`] so the browser can validate and
    /// offer the right keyboard, not because we store it differently.
    Url {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },

    /// A shared secret or verification token, such as the one a webhook is
    /// signed with.
    ///
    /// Masked on screen rather than shown, and revealable — a token that cannot
    /// be read back is a token nobody can check against the value they pasted
    /// into the provider, which is the one thing people actually need to do with
    /// it.
    Secret {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,

        /// Whether the browser may make up a value for this.
        ///
        /// Only for the secrets both sides merely have to agree on. A generator
        /// offered against one the provider issued would invite somebody to
        /// replace the only value that could ever work.
        #[serde(default)]
        generator: bool,

        /// How many random bytes a generated value carries, before encoding.
        #[serde(default = "default_generator_bytes")]
        generator_bytes: usize,
    },

    /// A number, optionally bounded.
    Number {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// The increment the control steps by, and so what counts as a whole
        /// value. A field stored as an integer says `1` here; without it the
        /// control would happily collect `1.5` for a priority and the save
        /// would fail on something the form had encouraged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
    },

    /// A yes or no.
    Boolean,

    /// A choice from a fixed list known when the descriptor is written.
    Select { options: Vec<OptionItem> },

    /// A choice from a list only the provider can supply — the projects in a
    /// Todoist account, say.
    ///
    /// The options are fetched from the connection named by `depends_on`, which
    /// is why that is not optional: a list of projects means nothing until we
    /// know whose projects. Until that field is set the control has nothing to
    /// offer and says so, rather than presenting an empty menu.
    Options {
        /// The named list to fetch, resolved by the connection's provider.
        source: String,
        /// The [`FieldKind::Connection`] field whose value scopes the lookup.
        depends_on: String,
        /// A further field whose value narrows the list, for the sources that
        /// are a list within something else — the sections of a project rather
        /// than of an account. Without this a picker could only ever be scoped
        /// by the account, and every section in the workspace would be offered
        /// as though it belonged to the chosen project.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },

    /// A linked account belonging to the signed-in user.
    Connection {
        /// The service the account must be with, e.g. `todoist`.
        provider: String,

        /// The credential shape the workflow needs when one provider offers
        /// more than one, such as GitHub PATs and App installations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connection_kind: Option<crate::ConnectionKind>,
    },

    /// A schedule. Rendered by a control that can explain what it means in
    /// words, because a cron expression is not something most people read
    /// fluently.
    Cron,

    /// A filter expression over the items the workflow collected.
    Filter {
        /// The names this workflow's items expose, so the editor can suggest
        /// them instead of leaving the user to guess.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<String>,
    },
}

impl FieldKind {
    /// The wire discriminator for this kind.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::TextArea { .. } => "text_area",
            Self::Url { .. } => "url",
            Self::Secret { .. } => "secret",
            Self::Number { .. } => "number",
            Self::Boolean => "boolean",
            Self::Select { .. } => "select",
            Self::Options { .. } => "options",
            Self::Connection { .. } => "connection",
            Self::Cron => "cron",
            Self::Filter { .. } => "filter",
        }
    }
}

/// Enough entropy that a generated secret is not the weakest part of what it
/// protects, while still being short enough to paste into a provider's own
/// settings by hand.
fn default_generator_bytes() -> usize {
    32
}

/// One value a workflow type asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldDescriptor {
    /// Where this value lives in the workflow's configuration, as a dotted path
    /// such as `todoist.project`.
    pub name: String,

    /// The label shown beside the control.
    pub label: String,

    /// A sentence explaining what the value is for, shown beneath the control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,

    pub kind: FieldKind,

    /// Whether the workflow cannot be saved without this.
    #[serde(default)]
    pub required: bool,

    /// The value a new workflow starts with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

impl FieldDescriptor {
    /// Declares a field with the given path, label and control.
    pub fn new(name: impl Into<String>, label: impl Into<String>, kind: FieldKind) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            help: None,
            kind,
            required: false,
            default: None,
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn with_default(mut self, default: impl Into<serde_json::Value>) -> Self {
        self.default = Some(default.into());
        self
    }
}

/// A kind of workflow that can be created, and the form that configures one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowTypeDescriptor {
    /// The stable identifier this type is stored under, e.g. `rss`.
    pub id: String,

    /// What to call this type in the UI.
    pub name: String,

    /// A sentence describing what workflows of this type do.
    pub description: String,

    /// The setup notes shown alongside the form, as Markdown.
    ///
    /// Markdown rather than a plain string because of what setting one of these
    /// up actually involves. Pointing a provider at a workflow means reading a
    /// paragraph of prose, following a link into that provider's own settings,
    /// and copying a filter expression that has to survive being read
    /// character-for-character. A `String` can carry the words but not the link
    /// or the code block, and a form that cannot show either sends people to
    /// search the web for the half of the instructions it left out.
    ///
    /// Every one of these is written here, in this crate, by whoever added the
    /// workflow type. None of it is ever supplied by a user, at any point —
    /// there is no field that collects it and no path that stores it. That is
    /// what makes it safe for the browser to render as HTML: the text is source
    /// code that shipped with the agent, so trusting it is the same act as
    /// trusting the rest of the binary.
    ///
    /// Distinct from [`WorkflowTypeDescriptor::description`], which is the
    /// one-line summary shown against this type in a menu of them. That one has
    /// to fit on a row; this one has to explain a setup. Collapsing them would
    /// mean either a menu of paragraphs or instructions of one sentence.
    ///
    /// Headings start at `##`, because the page supplies the title.
    #[serde(default)]
    pub documentation: String,

    pub trigger: WorkflowTrigger,

    pub fields: Vec<FieldDescriptor>,
}

/// A workflow as configured by its owner.
///
/// Unlike a connection, this carries its whole configuration rather than a
/// redacted summary. It can, because a workflow holds no credential: anything
/// secret is a linked account referenced by [`crate::ConnectionId`], which is
/// the reason connections were separated from workflows in the first place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub id: WorkflowId,

    /// The [`WorkflowTypeDescriptor::id`] describing this workflow's shape.
    #[serde(rename = "type")]
    pub type_id: String,

    /// What to call this workflow.
    ///
    /// Derived by the workflow type from the configuration rather than stored
    /// alongside it, because a feed named in two places is a feed that can be
    /// renamed in one of them. Types with nothing to draw on — the ones that can
    /// only sensibly exist once — fall back to the type's own name.
    pub name: String,

    /// Whether it currently runs. A paused workflow keeps its configuration and
    /// its history, which is what somebody who is debugging a noisy feed wants —
    /// deleting and recreating loses both.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// The values collected for this type's fields.
    ///
    /// Holds only what the workflow itself needs. The schedule is deliberately
    /// not in here: it belongs to the trigger rather than the work, and putting
    /// it alongside would mean every configuration type carrying a field its
    /// handler never reads.
    pub config: serde_json::Value,

    /// The schedule this runs on, for types triggered by [`WorkflowTrigger::Cron`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,

    /// The path a webhook-triggered workflow is reached at.
    ///
    /// Carried on the workflow rather than kept back like a connection's
    /// credential, because unlike a credential this one has to be readable: its
    /// owner has to paste it into the service that will call it, and a URL that
    /// can only be seen once is a URL that gets written down somewhere worse.
    /// Rotating it is how a leaked one is dealt with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_path: Option<String>,

    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,

    /// When this last ran, if it ever has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<chrono::DateTime<chrono::Utc>>,

    /// When this is next expected to run, for triggers that can say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run: Option<chrono::DateTime<chrono::Utc>>,

    /// How the most recent runs have gone, once there has been one.
    ///
    /// A summary rather than the runs themselves: a list of workflows carrying
    /// every one of their payloads is the saturation problem this replaced,
    /// moved somewhere else. The payloads are fetched per workflow, on demand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<crate::WorkflowHealth>,

    /// Whether this workflow remembers anything between runs that could be
    /// forgotten.
    ///
    /// Carried so the UI can offer to reset only the workflows a reset would do
    /// something to. A workflow that acts on whatever it is handed and keeps no
    /// watermark has nothing to clear, and offering the action anyway would make
    /// the one that does nothing indistinguishable from the one that re-files a
    /// year of history.
    #[serde(default)]
    pub resettable: bool,
}

fn default_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trigger_names_the_partition_holding_its_configurations() {
        assert_eq!(
            WorkflowTrigger::Cron {
                default_schedule: "@daily".into()
            }
            .partition(),
            "cron"
        );
        assert_eq!(
            WorkflowTrigger::Webhook {
                source: "github".into()
            }
            .partition(),
            "webhooks/github"
        );
        assert_eq!(
            WorkflowTrigger::RoutedWebhook {
                source: "github".into()
            }
            .partition(),
            "webhooks/github"
        );
    }

    #[test]
    fn a_trigger_is_tagged_by_kind_on_the_wire() {
        let json = serde_json::to_value(WorkflowTrigger::Webhook {
            source: "github".into(),
        })
        .unwrap();

        assert_eq!(json["kind"], "webhook");
        assert_eq!(json["source"], "github");

        let routed = serde_json::to_value(WorkflowTrigger::RoutedWebhook {
            source: "github".into(),
        })
        .unwrap();

        assert_eq!(routed["kind"], "routed_webhook");
        assert_eq!(routed["source"], "github");
    }

    #[test]
    fn every_field_kind_reports_the_discriminator_it_serializes_as() {
        let kinds = [
            FieldKind::Text { placeholder: None },
            FieldKind::TextArea { placeholder: None },
            FieldKind::Url { placeholder: None },
            FieldKind::Secret {
                placeholder: None,
                generator: false,
                generator_bytes: 32,
            },
            FieldKind::Number {
                min: None,
                max: None,
                step: None,
            },
            FieldKind::Boolean,
            FieldKind::Select { options: vec![] },
            FieldKind::Options {
                source: "projects".into(),
                depends_on: "todoist.connection".into(),
                parent: None,
            },
            FieldKind::Connection {
                provider: "todoist".into(),
                connection_kind: None,
            },
            FieldKind::Cron,
            FieldKind::Filter { fields: vec![] },
        ];

        for kind in kinds {
            let json = serde_json::to_value(&kind).unwrap();
            assert_eq!(
                json["kind"],
                kind.as_str(),
                "as_str disagreed with the serialized form of {kind:?}",
            );
        }
    }

    #[test]
    fn a_secret_without_a_generator_still_says_how_long_a_generated_one_would_be() {
        // The two travel together so the browser never has to invent a length:
        // a descriptor that asked for a generator but not a size would leave the
        // strength of every secret to whichever renderer drew it.
        let stored = serde_json::json!({ "kind": "secret" });

        let kind: FieldKind = serde_json::from_value(stored).unwrap();
        assert_eq!(
            kind,
            FieldKind::Secret {
                placeholder: None,
                generator: false,
                generator_bytes: 32,
            }
        );
    }

    #[test]
    fn a_dynamic_picker_always_names_the_connection_it_is_scoped_to() {
        // `depends_on` is not optional, so this is really a check that the type
        // cannot be built without it — the test exists to fail loudly if it is
        // ever relaxed to an Option for convenience.
        let kind = FieldKind::Options {
            source: "projects".into(),
            depends_on: "todoist.connection".into(),
            parent: None,
        };

        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["depends_on"], "todoist.connection");
    }

    #[test]
    fn a_descriptor_builds_up_from_its_required_parts() {
        let field = FieldDescriptor::new("url", "Feed URL", FieldKind::Url { placeholder: None })
            .with_help("The address of the feed to watch.")
            .required();

        assert_eq!(field.name, "url");
        assert_eq!(field.label, "Feed URL");
        assert!(field.required);
        assert_eq!(
            field.help.as_deref(),
            Some("The address of the feed to watch.")
        );
        assert!(field.default.is_none());
    }

    #[test]
    fn an_optional_field_leaves_no_trace_when_it_is_unset() {
        let field = FieldDescriptor::new("name", "Name", FieldKind::Text { placeholder: None });
        let json = serde_json::to_value(&field).unwrap();

        assert!(json.get("help").is_none());
        assert!(json.get("default").is_none());
        assert_eq!(json["required"], false);
    }

    #[test]
    fn a_type_carries_its_setup_notes_alongside_its_one_line_summary() {
        // The two are separate fields on the wire because they are shown in
        // different places: the summary in a menu of types, the notes beside the
        // form. A renderer that found only one of them would have to guess which.
        let descriptor = WorkflowTypeDescriptor {
            id: "rss".into(),
            name: "RSS Feed".into(),
            description: "Watches a feed and files a task for each new entry.".into(),
            documentation: "## Finding the feed\n\nUse the feed's own address.".into(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "@daily".into(),
            },
            fields: vec![],
        };

        let json = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(
            json["description"],
            "Watches a feed and files a task for each new entry.",
        );
        assert_eq!(
            json["documentation"],
            "## Finding the feed\n\nUse the feed's own address.",
        );
    }

    #[test]
    fn a_descriptor_written_before_setup_notes_existed_still_loads() {
        // Descriptors are served rather than compiled in, so a stored or cached
        // one from an older agent has to load with nothing to show rather than
        // failing and taking the whole list of types with it.
        let stored = serde_json::json!({
            "id": "rss",
            "name": "RSS Feed",
            "description": "Watches a feed and files a task for each new entry.",
            "trigger": { "kind": "cron", "default_schedule": "@daily" },
            "fields": [],
        });

        let descriptor: WorkflowTypeDescriptor = serde_json::from_value(stored).unwrap();
        assert!(descriptor.documentation.is_empty());
    }

    #[test]
    fn a_workflow_stored_before_pausing_existed_is_still_running() {
        // `enabled` was added after the first workflows were written, so a record
        // without it has to load as running rather than silently switching itself
        // off on upgrade.
        let stored = serde_json::json!({
            "id": "copper-tiger-canyon",
            "type": "rss",
            "name": "Citation Needed",
            "config": { "url": "https://example.com/feed" },
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
        });

        let workflow: Workflow = serde_json::from_value(stored).unwrap();
        assert!(workflow.enabled);
        assert!(workflow.last_run.is_none());
    }

    #[test]
    fn a_workflow_names_its_type_as_type_on_the_wire() {
        let workflow = Workflow {
            id: WorkflowId::from_entropy(1),
            type_id: "rss".into(),
            name: "Citation Needed".into(),
            enabled: true,
            config: serde_json::json!({}),
            schedule: Some("@daily".into()),
            webhook_path: None,
            resettable: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_run: None,
            next_run: None,
            health: None,
        };

        let json = serde_json::to_value(&workflow).unwrap();
        assert_eq!(json["type"], "rss");
        assert!(json.get("type_id").is_none());
    }
}
