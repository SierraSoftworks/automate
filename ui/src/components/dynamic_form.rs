//! Renders the form a workflow type describes.
//!
//! The agent says what a workflow type needs collected; this draws it. Nothing
//! here knows what an RSS feed is, which is the point — a workflow type added to
//! the agent gets a working form without this file changing.
//!
//! # Values are addressed by path
//!
//! A descriptor names its fields with dotted paths such as `todoist.project`.
//! The form holds one `serde_json::Value` and reads and writes into it at those
//! paths, so the object submitted has the shape the agent's own configuration
//! type expects without anything here having to know that shape.
//!
//! # Errors come from the agent
//!
//! Controls show an error but never decide one. What counts as valid depends on
//! the workflow type, which the agent is the only authority on, so a save that
//! is refused is reported against the field it names rather than pre-empted
//! here.

use std::collections::HashMap;
use std::rc::Rc;

use automate_api::{ConnectionSummary, FieldDescriptor, FieldKind, OptionItem};
use yew::prelude::*;

use crate::components::{Field, NumberInput, Select, SelectOption, Switch, TextArea, TextInput};

/// Reads the value at a dotted path, if there is one.
pub fn value_at<'a>(config: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.')
        .try_fold(config, |cursor, segment| cursor.get(segment))
        .filter(|value| !value.is_null())
}

/// Writes a value at a dotted path, creating the objects along the way.
///
/// A `None` removes the key rather than storing a null, so that a field left
/// empty is one the agent's own `#[serde(default)]` handles rather than one it
/// is asked to read a null out of.
pub fn set_at(config: &mut serde_json::Value, path: &str, value: Option<serde_json::Value>) {
    let mut segments: Vec<&str> = path.split('.').collect();
    let last = segments.pop().expect("a path always has a final segment");

    let mut cursor = config;
    for segment in segments {
        if !cursor.get(segment).map(|v| v.is_object()).unwrap_or(false) {
            cursor[segment] = serde_json::json!({});
        }
        cursor = cursor.get_mut(segment).expect("just ensured it is an object");
    }

    match value {
        Some(value) => cursor[last] = value,
        None => {
            if let Some(object) = cursor.as_object_mut() {
                object.remove(last);
            }
        }
    }
}

/// The value of a field as text, for the controls that work in text.
fn text_of(config: &serde_json::Value, path: &str) -> String {
    match value_at(config, path) {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// The choices a dynamic picker has fetched, keyed by the field that owns it.
pub type FetchedOptions = HashMap<String, Vec<OptionItem>>;

#[derive(Properties, PartialEq)]
pub struct DynamicFormProps {
    /// The fields to collect, in the order they should be shown.
    pub fields: Vec<FieldDescriptor>,

    /// The values collected so far.
    pub config: Rc<serde_json::Value>,

    /// Invoked with the whole configuration whenever any field changes.
    pub onchange: Callback<serde_json::Value>,

    /// The accounts this person has linked, for the connection pickers.
    #[prop_or_default]
    pub connections: Vec<ConnectionSummary>,

    /// Choices fetched from a provider, keyed by field name.
    #[prop_or_default]
    pub options: FetchedOptions,

    /// Field-level messages from a save the agent refused, keyed by field name.
    #[prop_or_default]
    pub errors: HashMap<String, String>,

    #[prop_or_default]
    pub disabled: bool,
}

#[function_component(DynamicForm)]
pub fn dynamic_form(props: &DynamicFormProps) -> Html {
    let fields = props.fields.iter().map(|descriptor| {
        html! {
            <DynamicField
                key={descriptor.name.clone()}
                descriptor={descriptor.clone()}
                config={props.config.clone()}
                onchange={props.onchange.clone()}
                connections={props.connections.clone()}
                options={props.options.get(&descriptor.name).cloned().unwrap_or_default()}
                error={props.errors.get(&descriptor.name).cloned()}
                disabled={props.disabled}
            />
        }
    });

    html! { <div class="dynamic-form">{ for fields }</div> }
}

#[derive(Properties, PartialEq)]
struct DynamicFieldProps {
    descriptor: FieldDescriptor,
    config: Rc<serde_json::Value>,
    onchange: Callback<serde_json::Value>,
    connections: Vec<ConnectionSummary>,
    options: Vec<OptionItem>,
    error: Option<String>,
    disabled: bool,
}

#[function_component(DynamicField)]
fn dynamic_field(props: &DynamicFieldProps) -> Html {
    let descriptor = &props.descriptor;
    let id = format!("field-{}", descriptor.name.replace('.', "-"));

    // Every control reports its new value the same way, so the update is written
    // once here rather than in each arm below.
    let update = {
        let (config, onchange, path) = (
            props.config.clone(),
            props.onchange.clone(),
            descriptor.name.clone(),
        );

        Callback::from(move |value: Option<serde_json::Value>| {
            let mut next = (*config).clone();
            set_at(&mut next, &path, value);
            onchange.emit(next);
        })
    };

    let text_update = {
        let update = update.clone();
        Callback::from(move |text: String| {
            let trimmed = text.trim().to_string();
            update.emit((!trimmed.is_empty()).then(|| serde_json::json!(trimmed)));
        })
    };

    let choice_update = {
        let update = update.clone();
        Callback::from(move |choice: Option<String>| {
            update.emit(choice.map(|value| serde_json::json!(value)));
        })
    };

    let value = text_of(&props.config, &descriptor.name);
    let invalid = props.error.is_some();

    let control = match &descriptor.kind {
        FieldKind::Text { placeholder } | FieldKind::Url { placeholder } => html! {
            <TextInput
                id={id.clone()}
                value={value.clone()}
                onchange={text_update}
                placeholder={placeholder.clone().map(AttrValue::from)}
                disabled={props.disabled}
                invalid={invalid}
            />
        },

        FieldKind::TextArea { placeholder } => html! {
            <TextArea
                id={id.clone()}
                value={value.clone()}
                onchange={text_update}
                placeholder={placeholder.clone().map(AttrValue::from)}
                disabled={props.disabled}
                invalid={invalid}
            />
        },

        FieldKind::Number { min, max, .. } => {
            let number_update = {
                let update = update.clone();
                Callback::from(move |number: Option<i64>| {
                    update.emit(number.map(|number| serde_json::json!(number)));
                })
            };

            html! {
                <NumberInput
                    id={id.clone()}
                    value={value_at(&props.config, &descriptor.name).and_then(|v| v.as_i64())}
                    onchange={number_update}
                    min={min.map(|min| min as i64)}
                    max={max.map(|max| max as i64)}
                    disabled={props.disabled}
                />
            }
        }

        FieldKind::Boolean => {
            let switch_update = {
                let update = update.clone();
                Callback::from(move |on: bool| update.emit(Some(serde_json::json!(on))))
            };

            html! {
                <Switch
                    id={id.clone()}
                    checked={value_at(&props.config, &descriptor.name).and_then(|v| v.as_bool()).unwrap_or(false)}
                    onchange={switch_update}
                    disabled={props.disabled}
                />
            }
        }

        FieldKind::Select { options } => html! {
            <Select
                id={id.clone()}
                value={(!value.is_empty()).then(|| AttrValue::from(value.clone()))}
                onchange={choice_update}
                options={options.iter().map(|o| SelectOption::new(o.value.clone(), o.label.clone())).collect::<Vec<_>>()}
                clearable={!descriptor.required}
                disabled={props.disabled}
            />
        },

        FieldKind::Connection { provider } => {
            let choices: Vec<SelectOption> = props
                .connections
                .iter()
                .filter(|connection| &connection.provider == provider)
                .map(|connection| {
                    SelectOption::new(connection.id.to_string(), connection.name.clone())
                })
                .collect();

            if choices.is_empty() {
                // Offering an empty menu would look like a bug in the form
                // rather than a step the person has not taken yet.
                html! {
                    <p class="dynamic-form__unavailable">
                        { format!("Link a {provider} account before you can use this.") }
                    </p>
                }
            } else {
                html! {
                    <Select
                        id={id.clone()}
                        value={(!value.is_empty()).then(|| AttrValue::from(value.clone()))}
                        onchange={choice_update}
                        options={choices}
                        clearable={!descriptor.required}
                        disabled={props.disabled}
                    />
                }
            }
        }

        FieldKind::Options { depends_on, .. } => {
            let scoped_by = value_at(&props.config, depends_on).is_some();

            if !scoped_by {
                // The choices come from an account, so until one is picked there
                // is nothing to offer. Saying so is more use than an empty menu
                // that looks broken.
                html! {
                    <p class="dynamic-form__unavailable">
                        { "Choose an account first, and the options will be loaded from it." }
                    </p>
                }
            } else {
                // A value stored before the list was fetched, or one since
                // removed at the provider, is kept as a choice so the control
                // does not silently appear to be set to something else.
                let mut choices: Vec<SelectOption> = props
                    .options
                    .iter()
                    .map(|option| SelectOption::new(option.value.clone(), option.label.clone()))
                    .collect();

                if !value.is_empty() && !choices.iter().any(|choice| choice.value == value) {
                    choices.insert(
                        0,
                        SelectOption::new(value.clone(), format!("{value} (not in this account)")),
                    );
                }

                html! {
                    <Select
                        id={id.clone()}
                        value={(!value.is_empty()).then(|| AttrValue::from(value.clone()))}
                        onchange={choice_update}
                        options={choices}
                        placeholder={if props.options.is_empty() { "Loading…" } else { "Choose one" }}
                        clearable={!descriptor.required}
                        disabled={props.disabled}
                    />
                }
            }
        }

        FieldKind::Cron => html! {
            <TextInput
                id={id.clone()}
                value={value.clone()}
                onchange={text_update}
                placeholder={Some(AttrValue::from("@daily"))}
                disabled={props.disabled}
                invalid={invalid}
            />
        },

        FieldKind::Filter { fields } => html! {
            <>
                <TextInput
                    id={id.clone()}
                    value={value.clone()}
                    onchange={text_update}
                    placeholder={Some(AttrValue::from("title contains \"release\""))}
                    disabled={props.disabled}
                    invalid={invalid}
                />
                if !fields.is_empty() {
                    <p class="dynamic-form__hint">
                        { "You can match on: " }
                        { fields.join(", ") }
                    </p>
                }
            </>
        },
    };

    // The help text a descriptor carries explains the field; a cron field also
    // gets a plain reading of the schedule, which is the part most people cannot
    // do at a glance.
    let help = match (&descriptor.kind, &descriptor.help) {
        (FieldKind::Cron, help) => {
            let described = crate::util::describe_cron(&value);
            Some(AttrValue::from(match (help, described) {
                (Some(help), Some(described)) => format!("{help} Currently: {described}."),
                (Some(help), None) => help.clone(),
                (None, Some(described)) => format!("Runs {described}."),
                (None, None) => String::new(),
            }))
        }
        (_, help) => help.clone().map(AttrValue::from),
    }
    .filter(|help| !help.is_empty());

    html! {
        <Field
            label={descriptor.label.clone()}
            id={id}
            required={descriptor.required}
            help={help}
            error={props.error.clone().map(AttrValue::from)}
        >
            { control }
        </Field>
    }
}
