//! The registry of workflow types that can be created from the API.
//!
//! A workflow type is a [`Job`] that additionally knows how to describe itself:
//! what it is called, what it does, and what it needs to be told. That
//! description is what lets a browser offer "add an RSS feed" without anybody
//! having written an RSS form, and it is why adding a workflow type is a change
//! to this crate alone.
//!
//! # Validation comes from the job, not a second schema
//!
//! [`WorkflowType::validate`] checks a submitted configuration by deserializing
//! it into the very type the handler is given. It deliberately does not consult
//! the descriptor. A descriptor is a description of a form; the handler's type
//! is the thing that actually has to hold the value, so it is the only
//! authority worth having. Checking against the descriptor instead would create
//! a second schema that agrees with the first right up until somebody edits one
//! of them.
//!
//! The consequence worth stating plainly: a configuration is rejected when it is
//! saved rather than when it runs. A workflow that is stored is a workflow that
//! at least deserializes, so a run can fail on the network or the far end but
//! not on its own configuration.

use std::collections::HashMap;
use std::sync::LazyLock;

use automate_api::WorkflowTypeDescriptor;
use human_errors::Error;

use crate::job::Job;

/// A [`Job`] that a user can create instances of from the API.
pub trait ConfigurableWorkflow: Job {
    /// The stable identifier this type is stored under, e.g. `rss`.
    ///
    /// Distinct from [`Job::partition`] because a partition is a routing detail
    /// that has been renamed before (see migration 6) whereas this ends up
    /// inside stored records, where a rename is a migration.
    fn type_id() -> &'static str;

    /// The form that configures one of these.
    fn descriptor() -> WorkflowTypeDescriptor;

    /// What to call a particular instance, drawn from its configuration.
    ///
    /// Defaults to the type's own name, which suits the workflows that can only
    /// sensibly exist once.
    fn describe(config: &Self::JobType) -> String {
        let _ = config;
        Self::descriptor().name
    }
}

/// The type-erased view of a workflow type, so that the registry can hold every
/// type in one map without knowing any of their configuration types.
pub trait WorkflowType: Send + Sync {
    fn type_id(&self) -> &'static str;

    fn descriptor(&self) -> WorkflowTypeDescriptor;

    /// The queue partition an instance of this type is dispatched into.
    fn partition(&self) -> &'static str;

    /// Whether this configuration is one the handler could actually run.
    fn validate(&self, config: &serde_json::Value) -> Result<(), Error>;

    /// What to call the instance this configuration describes.
    fn describe(&self, config: &serde_json::Value) -> Result<String, Error>;
}

impl<W> WorkflowType for W
where
    W: ConfigurableWorkflow + Send + Sync + 'static,
{
    fn type_id(&self) -> &'static str {
        <W as ConfigurableWorkflow>::type_id()
    }

    fn descriptor(&self) -> WorkflowTypeDescriptor {
        <W as ConfigurableWorkflow>::descriptor()
    }

    fn partition(&self) -> &'static str {
        <W as Job>::partition()
    }

    fn validate(&self, config: &serde_json::Value) -> Result<(), Error> {
        self.deserialize(config).map(|_| ())
    }

    fn describe(&self, config: &serde_json::Value) -> Result<String, Error> {
        Ok(<W as ConfigurableWorkflow>::describe(
            &self.deserialize(config)?,
        ))
    }
}

/// Shared by [`WorkflowType::validate`] and [`WorkflowType::describe`], so that
/// the two cannot disagree about what a valid configuration is.
trait DeserializeConfig: ConfigurableWorkflow {
    fn deserialize(&self, config: &serde_json::Value) -> Result<Self::JobType, Error> {
        <Self::JobType as serde::Deserialize>::deserialize(config).map_err(|err| {
            // serde's message names the offending field, which is the one thing
            // somebody fixing this actually needs, so it is passed through
            // rather than replaced with something tidier and less useful.
            human_errors::user(
                format!(
                    "This {} workflow is not configured correctly: {err}",
                    Self::type_id()
                ),
                &[
                    "Check that every required field has been filled in.",
                    "Check that each field holds the kind of value it asks for.",
                ],
            )
        })
    }
}

impl<W: ConfigurableWorkflow> DeserializeConfig for W {}

/// A registration entry for a [`WorkflowType`], collected by [`inventory`].
/// Use [`register_workflow_type!`] to submit one.
pub struct WorkflowTypeRegistration(&'static dyn WorkflowType);

impl WorkflowTypeRegistration {
    pub const fn new<T: WorkflowType>(workflow: &'static T) -> Self {
        Self(workflow)
    }

    pub fn workflow(&self) -> &'static dyn WorkflowType {
        self.0
    }
}

inventory::collect!(WorkflowTypeRegistration);

/// The dotted path to a field, checked at compile time against the type that
/// holds it.
///
/// A descriptor addresses values by string path, which is what the renderer
/// needs but is also a string nothing was keeping honest: renaming a field on
/// the configuration struct left the descriptor advertising a name that no
/// longer existed, and the form went on collecting a value serde would then
/// discard. This borrows the field it names, so that rename stops compiling.
///
/// ```ignore
/// config_path!(RssConfig: todoist.project) // => "todoist.project"
/// ```
///
/// What this does *not* prove is that every field the configuration requires
/// has a descriptor. Rust cannot enumerate a struct's fields without a derive
/// macro, so that direction is covered by a test which builds a configuration
/// out of the descriptor and asserts the handler's own type accepts it.
#[macro_export]
macro_rules! config_path {
    ($ty:ty : $head:ident $(. $rest:ident)*) => {{
        #[allow(unused)]
        fn assert_field_exists(value: &$ty) {
            let _ = &value.$head $(. $rest)*;
        }

        concat!(stringify!($head) $(, ".", stringify!($rest))*)
    }};
}

/// Registers a [`ConfigurableWorkflow`] so that it can be created from the API.
///
/// This is separate from [`crate::register_job!`] because the two answer
/// different questions: that one says work of this kind can be run, this one
/// says work of this kind can be asked for. Plenty of jobs are only ever
/// dispatched by other jobs and have no business appearing in a menu.
#[macro_export]
macro_rules! register_workflow_type {
    ($workflow:expr) => {
        inventory::submit! { $crate::workflows::WorkflowTypeRegistration::new(&$workflow) }
    };
}

/// Every registered workflow type, keyed by its identifier.
///
/// Built once. A duplicate identifier is a programming error that would
/// otherwise silently shadow one type with another, so it panics here rather
/// than surfacing later as a workflow that saves and never runs.
pub fn registry() -> &'static HashMap<&'static str, &'static dyn WorkflowType> {
    static REGISTRY: LazyLock<HashMap<&'static str, &'static dyn WorkflowType>> = LazyLock::new(
        || {
            let mut registry: HashMap<&'static str, &'static dyn WorkflowType> = HashMap::new();

            for registration in inventory::iter::<WorkflowTypeRegistration> {
                let workflow = registration.workflow();
                if registry.insert(workflow.type_id(), workflow).is_some() {
                    panic!(
                        "Two workflow types are registered as '{}'. Each type needs its own identifier.",
                        workflow.type_id()
                    );
                }
            }

            registry
        },
    );

    &REGISTRY
}

/// Looks up a workflow type by identifier.
pub fn lookup(type_id: &str) -> Result<&'static dyn WorkflowType, Error> {
    registry().get(type_id).copied().ok_or_else(|| {
        human_errors::user(
            format!("There is no workflow type called '{type_id}'."),
            &[
                "Check the identifier against the list of available workflow types.",
                "If this workflow used to work, it may have been removed in an upgrade.",
            ],
        )
    })
}

/// Every workflow type's description, ordered by name so that a menu built from
/// this does not reshuffle itself between requests.
pub fn descriptors() -> Vec<WorkflowTypeDescriptor> {
    let mut descriptors: Vec<_> = registry()
        .values()
        .map(|workflow| workflow.descriptor())
        .collect();
    descriptors.sort_by(|a, b| a.name.cmp(&b.name));
    descriptors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_type_has_a_unique_identifier() {
        // `registry()` panics on a duplicate, so building it is the assertion.
        let registry = registry();
        assert!(
            !registry.is_empty(),
            "no workflow types are registered, so nothing could be created from the API",
        );
    }

    #[test]
    fn a_types_descriptor_agrees_with_the_identifier_it_is_registered_under() {
        for (type_id, workflow) in registry() {
            assert_eq!(
                &workflow.descriptor().id,
                type_id,
                "the descriptor for '{type_id}' names a different id, so a form submitted from it would be saved as the wrong type",
            );
        }
    }

    #[test]
    fn a_types_fields_are_uniquely_named() {
        for (type_id, workflow) in registry() {
            let descriptor = workflow.descriptor();
            let mut seen = std::collections::HashSet::new();
            for field in &descriptor.fields {
                assert!(
                    seen.insert(field.name.clone()),
                    "'{type_id}' asks for '{}' twice, so one of the two would silently overwrite the other",
                    field.name,
                );
            }
        }
    }

    #[test]
    fn a_dynamic_picker_depends_on_a_field_its_type_actually_has() {
        for (type_id, workflow) in registry() {
            let descriptor = workflow.descriptor();
            let names: std::collections::HashSet<_> =
                descriptor.fields.iter().map(|f| f.name.as_str()).collect();

            for field in &descriptor.fields {
                if let automate_api::FieldKind::Options { depends_on, .. } = &field.kind {
                    assert!(
                        names.contains(depends_on.as_str()),
                        "'{type_id}' has a picker '{}' scoped to '{depends_on}', which is not one of its fields, so it could never be filled in",
                        field.name,
                    );
                }
            }
        }
    }

    #[test]
    fn looking_up_an_unknown_type_says_so_rather_than_panicking() {
        let Err(err) = lookup("not-a-real-workflow-type") else {
            panic!("an unknown workflow type should not resolve to a handler");
        };
        assert!(
            format!("{err}").contains("not-a-real-workflow-type"),
            "the error should name the type that was asked for: {err}",
        );
    }

    #[test]
    fn a_type_names_an_instance_from_its_own_configuration() {
        let rss = lookup("rss").unwrap();

        let name = rss
            .describe(&serde_json::json!({
                "name": "Citation Needed",
                "url": "https://example.com/rss/",
                "homepage": "https://example.com/",
            }))
            .unwrap();

        assert_eq!(name, "Citation Needed");
    }

    #[test]
    fn a_type_falls_back_to_its_own_name_when_an_instance_has_nothing_to_add() {
        // Types that can only sensibly exist once have no name field to draw on,
        // so the type's name is the only sensible label for the instance.
        struct Unnamed;
        impl Job for Unnamed {
            type JobType = ();
            fn partition() -> &'static str {
                "test/unnamed"
            }
            async fn handle(
                &self,
                _ctx: crate::job::JobContext<
                    impl crate::services::Services + Send + Sync + 'static,
                >,
                _job: &Self::JobType,
            ) -> Result<(), Error> {
                Ok(())
            }
        }
        impl ConfigurableWorkflow for Unnamed {
            fn type_id() -> &'static str {
                "test-unnamed"
            }
            fn descriptor() -> WorkflowTypeDescriptor {
                WorkflowTypeDescriptor {
                    id: <Self as ConfigurableWorkflow>::type_id().to_string(),
                    name: "Unnamed".to_string(),
                    description: String::new(),
                    trigger: automate_api::WorkflowTrigger::Cron {
                        default_schedule: "@daily".to_string(),
                    },
                    fields: vec![],
                }
            }
        }

        assert_eq!(
            WorkflowType::describe(&Unnamed, &serde_json::json!(null)).unwrap(),
            "Unnamed",
        );
    }

    #[test]
    fn a_configuration_the_handler_could_not_read_is_refused() {
        let rss = lookup("rss").unwrap();

        // `url` is missing, so this would deserialize into nothing the handler
        // could run.
        let Err(err) = rss.validate(&serde_json::json!({
            "name": "Broken",
            "homepage": "https://example.com/",
        })) else {
            panic!("a configuration missing a required field should not validate");
        };

        assert!(
            format!("{err}").contains("url"),
            "the error should name the field at fault so it can be fixed: {err}",
        );
    }

    #[test]
    fn every_type_dispatches_into_a_partition_something_handles() {
        // A type whose partition has no registered handler would save happily,
        // schedule happily, and then have every run dropped as unroutable.
        let handled: std::collections::HashSet<_> = inventory::iter::<crate::job::JobRegistration>
            .into_iter()
            .map(|registration| registration.handler().partition())
            .collect();

        for (type_id, workflow) in registry() {
            assert!(
                handled.contains(workflow.partition()),
                "'{type_id}' dispatches into '{}', which no job handles, so its runs would be dropped",
                workflow.partition(),
            );
        }
    }

    /// Builds a value the given field could plausibly have collected.
    ///
    /// The default is tried first and the placeholder second, because both are
    /// things the descriptor is already claiming a person could enter — so a
    /// default or an example that the handler would reject is itself the bug,
    /// and this notices. Only when a field offers neither does the kind decide,
    /// and then only its shape matters rather than its realism.
    fn synthetic_value(field: &automate_api::FieldDescriptor) -> serde_json::Value {
        if let Some(default) = &field.default {
            return default.clone();
        }

        if let Some(placeholder) = placeholder_of(&field.kind) {
            return serde_json::json!(placeholder);
        }

        kind_shaped_value(&field.kind)
    }

    /// The example a control shows when it is empty, where it has one.
    fn placeholder_of(kind: &automate_api::FieldKind) -> Option<&str> {
        use automate_api::FieldKind;

        match kind {
            FieldKind::Text { placeholder }
            | FieldKind::TextArea { placeholder }
            | FieldKind::Url { placeholder } => placeholder.as_deref(),
            _ => None,
        }
    }

    /// A value of the right shape for a control that offered no example.
    fn kind_shaped_value(kind: &automate_api::FieldKind) -> serde_json::Value {
        use automate_api::FieldKind;

        match kind {
            FieldKind::Text { .. } | FieldKind::TextArea { .. } => serde_json::json!("example"),
            FieldKind::Url { .. } => serde_json::json!("https://example.com/"),
            // Emitted as an integer when it is a whole number, since that is
            // what a field stored as one will accept and a field stored as a
            // decimal will accept too.
            FieldKind::Number { min, .. } => match min.unwrap_or(1.0) {
                value if value.fract() == 0.0 => serde_json::json!(value as i64),
                value => serde_json::json!(value),
            },
            FieldKind::Boolean => serde_json::json!(false),
            FieldKind::Select { options } => options
                .first()
                .map(|option| serde_json::json!(option.value))
                .unwrap_or(serde_json::json!("example")),
            FieldKind::Options { .. } => serde_json::json!("example"),
            FieldKind::Connection { .. } => {
                serde_json::json!(automate_api::ConnectionId::from_entropy(0).to_string())
            }
            FieldKind::Cron => serde_json::json!("@daily"),
            // A filter has to parse, so the only value guaranteed to is the one
            // the type produces for itself.
            FieldKind::Filter { .. } => {
                serde_json::to_value(crate::filter::Filter::default()).unwrap()
            }
        }
    }

    /// Writes `value` into `target` at a dotted path, creating objects as needed.
    fn insert_at(target: &mut serde_json::Value, path: &str, value: serde_json::Value) {
        let mut cursor = target;
        let mut segments = path.split('.').peekable();

        while let Some(segment) = segments.next() {
            if segments.peek().is_none() {
                cursor[segment] = value;
                return;
            }

            if !cursor[segment].is_object() {
                cursor[segment] = serde_json::json!({});
            }
            cursor = &mut cursor[segment];
        }
    }

    fn config_from(
        fields: &[automate_api::FieldDescriptor],
        required_only: bool,
    ) -> serde_json::Value {
        let mut config = serde_json::json!({});
        for field in fields {
            if required_only && !field.required {
                continue;
            }
            insert_at(&mut config, &field.name, synthetic_value(field));
        }
        config
    }

    #[test]
    fn a_form_filled_in_completely_produces_a_configuration_the_handler_accepts() {
        // `config_path!` proves each descriptor names a field that exists. This
        // is the other direction: that the fields it names are *enough*, and
        // hold what the handler's type expects. Between them, a descriptor
        // cannot describe a form which collects a configuration that will not
        // load.
        for (type_id, workflow) in registry() {
            let descriptor = workflow.descriptor();
            let config = config_from(&descriptor.fields, false);

            if let Err(err) = workflow.validate(&config) {
                panic!(
                    "'{type_id}' describes a form whose completed values its handler rejects: {err}\nconfiguration was: {config:#}",
                );
            }
        }
    }

    #[test]
    fn a_form_with_only_its_required_fields_filled_in_is_enough_to_save() {
        // Anything the handler insists on must be marked required, or somebody
        // filling in the form as instructed would be told at the last moment
        // that it cannot be saved.
        for (type_id, workflow) in registry() {
            let descriptor = workflow.descriptor();
            let config = config_from(&descriptor.fields, true);

            if let Err(err) = workflow.validate(&config) {
                panic!(
                    "'{type_id}' has a field its handler requires but its form does not mark required: {err}\nconfiguration was: {config:#}",
                );
            }
        }
    }

    #[test]
    fn the_workflows_a_person_would_want_to_create_are_all_offered() {
        // Named individually rather than counted, so that removing one is a
        // decision somebody has to make here rather than a number quietly going
        // down.
        for expected in [
            "rss",
            "calendar",
            "youtube",
            "github-releases",
            "xkcd",
            "ynab-stocks",
        ] {
            assert!(
                registry().contains_key(expected),
                "'{expected}' is not offered, so nobody could create one",
            );
        }
    }

    #[test]
    fn the_installations_own_maintenance_is_not_offered_as_something_to_create() {
        // These run on a schedule the installation decides, and exist so that
        // records the other workflows leave behind get tidied up. Offering them
        // would invite somebody to configure away their own housekeeping, and
        // there is nothing about them a person would sensibly choose.
        for internal in [
            "todoist-cleanup",
            "github-notifications-cleanup",
            "github-notifications",
        ] {
            assert!(
                !registry().contains_key(internal),
                "'{internal}' is the installation's own work and should not be offered as a workflow",
            );
        }
    }

    #[test]
    fn descriptors_are_listed_in_a_stable_order() {
        let first = descriptors();
        let second = descriptors();
        assert_eq!(
            first.iter().map(|d| &d.id).collect::<Vec<_>>(),
            second.iter().map(|d| &d.id).collect::<Vec<_>>(),
        );
    }
}
