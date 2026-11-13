//! The specimens the gallery renders, and the registry that names them.
//!
//! Each entry is a control and the states worth looking at side by side. Where a
//! control is interactive its specimen owns real state, so it can be typed into
//! and toggled rather than merely looked at — a disabled-looking control and a
//! control that ignores input are indistinguishable in a screenshot.

use std::collections::HashMap;
use std::rc::Rc;

use automate_api::{FieldDescriptor, FieldKind, OptionItem};
use serde_json::json;
use yew::prelude::*;

use yewi::prelude::*;

use crate::components::form::{ChoiceSelect, SelectOption};
use crate::components::{
    BrowserEntry, BrowserPartition, ConnectMenu, ConnectionsPanel, DbEntity, Documentation,
    DynamicForm, EntityMetadata, FilterInput, ImpersonationNotice, PartitionBrowser, RefreshButton,
    WebhookAddress,
};
use crate::fixtures;

use super::Specimen;

/// One entry in the gallery.
pub struct Control {
    /// The URL segment this control is reached at.
    pub slug: &'static str,
    /// What it is called in the navigation and heading.
    pub name: &'static str,
    /// One line on what the control is for.
    pub blurb: &'static str,
    /// Renders its specimens.
    pub view: fn() -> Html,
}

/// Every control the gallery covers. Buttons, fields, alerts and the rest of the
/// primitives come from yewi and are documented on its own reference site, so
/// what is listed here is only what Automate builds out of them.
pub const CONTROLS: &[Control] = &[
    Control {
        slug: "select",
        name: "Select",
        blurb: "A choice from a list the agent supplied, including a stored value that is no longer offered.",
        view: || html! { <Selects /> },
    },
    Control {
        slug: "filter-input",
        name: "Filter input",
        blurb: "A filter expression, with the diagnostics it offers as you type.",
        view: || html! { <FilterInputs /> },
    },
    Control {
        slug: "db-entity",
        name: "Entity",
        blurb: "One stored record, collapsed to its key until it is opened.",
        view: || html! { <Entities /> },
    },
    Control {
        slug: "partition-browser",
        name: "Partition browser",
        blurb: "The master/detail view the data page is built from.",
        view: || html! { <Browsers /> },
    },
    Control {
        slug: "dynamic-form",
        name: "Dynamic form",
        blurb: "Every field kind a workflow type can ask for, drawn from a descriptor.",
        view: || html! { <DynamicForms /> },
    },
    Control {
        slug: "documentation",
        name: "Documentation",
        blurb: "The setup guidance a workflow type ships with, as rendered Markdown.",
        view: || html! { <Documentations /> },
    },
    Control {
        slug: "webhook-address",
        name: "Webhook address",
        blurb: "The address a webhook workflow is reached at, and rotating it.",
        view: || html! { <WebhookAddresses /> },
    },
    Control {
        slug: "toolbar",
        name: "Toolbar controls",
        blurb: "The refresh button and the connect menu, as they appear in a title row.",
        view: || html! { <Toolbars /> },
    },
    Control {
        slug: "connections-panel",
        name: "Connections panel",
        blurb: "Each configured integration and the accounts connected to it.",
        view: || html! { <ConnectionsPanels /> },
    },
    Control {
        slug: "impersonation-notice",
        name: "Impersonation notice",
        blurb: "The strip saying whose records are on screen while acting as somebody.",
        view: || html! { <ImpersonationNotices /> },
    },
];

/// Binds a state handle to a control's `onchange`, which is the whole of what
/// most of these specimens need to be interactive.
fn bind(state: &UseStateHandle<String>) -> Callback<String> {
    let state = state.clone();
    Callback::from(move |value: String| state.set(value))
}

/// A callback that does nothing, for the specimens whose action has no meaning
/// outside a page.
fn inert<T: 'static>() -> Callback<T> {
    Callback::from(|_| ())
}

#[function_component(ImpersonationNotices)]
fn impersonation_notices() -> Html {
    html! {
        <>
            <Specimen
                label="Acting as somebody"
                note="Shown across the top of the shell rather than inside the page, because \
                      it changes what every other screen means — and the way out of it has \
                      to be where somebody looking for it will look."
            >
                <ImpersonationNotice account="rmuir" onstop={inert()} />
            </Specimen>

            <Specimen
                label="Acting as the installation"
                note="The account that owns everything configured before people were told \
                      apart. Nobody can sign into it, so this is the only way back to it."
            >
                <ImpersonationNotice account="!local" onstop={inert()} />
            </Specimen>
        </>
    }
}

#[function_component(Selects)]
fn selects() -> Html {
    let chosen = use_state(|| None::<String>);
    let clearable = use_state(|| Some("2203306141".to_string()));

    let options: Vec<SelectOption> = fixtures::connection_options("projects", None)
        .into_iter()
        .map(|item| SelectOption::new(item.value, item.label))
        .collect();

    let on_chosen = {
        let chosen = chosen.clone();
        Callback::from(move |value: Option<String>| chosen.set(value))
    };
    let on_clearable = {
        let clearable = clearable.clone();
        Callback::from(move |value: Option<String>| clearable.set(value))
    };

    html! {
        <>
            <Specimen label="Nothing chosen">
                <ChoiceSelect
                    value={(*chosen).clone().map(AttrValue::from)}
                    onchange={on_chosen}
                    options={options.clone()}
                    placeholder="Choose a project"
                />
            </Specimen>

            <Specimen label="Clearable">
                <ChoiceSelect
                    value={(*clearable).clone().map(AttrValue::from)}
                    onchange={on_clearable}
                    options={options.clone()}
                    placeholder="Any project"
                    clearable=true
                />
            </Specimen>

            <Specimen
                label="Stored value no longer offered"
                note="A project renamed at the provider. Showing it keeps the field honest about \
                      what is stored, rather than appearing to be set to the first entry."
            >
                <ChoiceSelect
                    value={Some(AttrValue::from("2203306999"))}
                    onchange={inert()}
                    options={options.clone()}
                />
            </Specimen>

            <Specimen label="Invalid and disabled">
                <div class="specimen__row">
                    <ChoiceSelect
                        value={None}
                        onchange={inert()}
                        options={options.clone()}
                        invalid=true
                    />
                    <ChoiceSelect
                        value={Some(AttrValue::from("2203306140"))}
                        onchange={inert()}
                        options={options}
                        disabled=true
                    />
                </div>
            </Specimen>
        </>
    }
}

#[function_component(FilterInputs)]
fn filter_inputs() -> Html {
    let empty = use_state(String::new);
    let valid = use_state(|| "title contains \"release\"".to_string());
    let unknown = use_state(|| "headline contains \"release\"".to_string());
    let broken = use_state(|| "title contains".to_string());

    let fields = vec![
        "title".to_string(),
        "link".to_string(),
        "summary".to_string(),
        "author".to_string(),
    ];

    html! {
        <>
            <Specimen label="Empty" note="Says nothing until there is something to say about.">
                <FilterInput
                    value={(*empty).clone()}
                    onchange={bind(&empty)}
                    fields={fields.clone()}
                />
            </Specimen>

            <Specimen label="Valid">
                <FilterInput
                    value={(*valid).clone()}
                    onchange={bind(&valid)}
                    fields={fields.clone()}
                />
            </Specimen>

            <Specimen
                label="References a field this workflow does not expose"
                note="A warning rather than an error: the expression parses, it just cannot match."
            >
                <FilterInput
                    value={(*unknown).clone()}
                    onchange={bind(&unknown)}
                    fields={fields.clone()}
                />
            </Specimen>

            <Specimen label="Does not parse">
                <FilterInput
                    value={(*broken).clone()}
                    onchange={bind(&broken)}
                    fields={fields.clone()}
                />
            </Specimen>

            <Specimen label="Refused by the agent, and disabled">
                <div class="specimen__stack">
                    <FilterInput
                        value={(*valid).clone()}
                        onchange={bind(&valid)}
                        fields={fields.clone()}
                        invalid=true
                    />
                    <FilterInput
                        value={(*valid).clone()}
                        onchange={inert()}
                        {fields}
                        disabled=true
                    />
                </div>
            </Specimen>
        </>
    }
}

#[function_component(Entities)]
fn entities() -> Html {
    html! {
        <>
            <Specimen label="Key and payload only">
                <DbEntity
                    partition="rss_state"
                    entity_key="https://example.com/feed.xml"
                    payload={json!({ "last_seen": "2024-05-01T12:00:00Z", "etag": "\"a1b2c3\"" })}
                />
            </Specimen>

            <Specimen
                label="With metadata and controls"
                note="The key, the always-visible meta line and the controls stay put; the \
                      payload and the labelled facts are revealed on expanding."
            >
                <DbEntity
                    partition="github_notifications"
                    entity_key="notif-7781"
                    meta={html! { <span class="db-entity__meta">{ "delayed · available in 4m" }</span> }}
                    metadata={vec![
                        EntityMetadata::new("Trace", "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
                        EntityMetadata::new("Scheduled", "2026-06-08T12:48:38Z"),
                    ]}
                    payload={json!({ "action": "archive", "thread": 7781 })}
                >
                    <Button variant={ButtonVariant::Subtle} size={Size::Small}>{ "Trigger" }</Button>
                    <Button variant={ButtonVariant::Danger} size={Size::Small}>{ "Delete" }</Button>
                </DbEntity>
            </Specimen>
        </>
    }
}

#[function_component(Browsers)]
fn browsers() -> Html {
    let partition =
        |name: &'static str, kind: &'static str, entries: Vec<BrowserEntry>| BrowserPartition {
            id: AttrValue::from(format!("{kind}:{name}")),
            name: AttrValue::from(name),
            kind: AttrValue::from(kind),
            icon: icons::DATABASE,
            entries,
        };

    let entry = |key: &str, payload: serde_json::Value| BrowserEntry {
        key: AttrValue::from(key.to_string()),
        search: AttrValue::from(key.to_lowercase()),
        content: html! {
            <DbEntity partition="demo" entity_key={key.to_string()} payload={payload} />
        },
    };

    let partitions = vec![
        partition(
            "rss_state",
            "kv",
            vec![
                entry(
                    "https://example.com/feed.xml",
                    json!({ "last_seen": "2024-05-01T12:00:00Z" }),
                ),
                entry(
                    "https://blog.sierrasoftworks.com/feed.xml",
                    json!({ "last_seen": "2026-06-08T09:12:00Z" }),
                ),
            ],
        ),
        partition(
            "todoist_create",
            "queue",
            vec![entry(
                "task-001",
                json!({ "content": "Review the deployment runbook" }),
            )],
        ),
    ];

    html! {
        <>
            <Specimen
                label="Populated"
                note="The sidebar and the entries are narrowed together by the shared search \
                      filter, which this page does not provide — so nothing is filtered here."
            >
                <PartitionBrowser {partitions} />
            </Specimen>

            <Specimen label="Empty">
                <PartitionBrowser
                    partitions={Vec::<BrowserPartition>::new()}
                    empty="Nothing is stored yet."
                />
            </Specimen>
        </>
    }
}

#[function_component(DynamicForms)]
fn dynamic_forms() -> Html {
    let connections = fixtures::service_connections();

    // Seeded with a linked account, because an unset connection is what puts
    // the dependent picker into its "choose an account first" state — worth
    // seeing, but not at the cost of never seeing the picker populated.
    let chosen = connections
        .iter()
        .find(|connection| connection.provider == "todoist")
        .map(|connection| connection.id.to_string())
        .unwrap_or_default();

    let config = use_state(|| {
        Rc::new(json!({
            "name": "Sierra Softworks blog",
            "feed": { "url": "https://blog.sierrasoftworks.com/feed.xml" },
            "notes": "Only the release announcements.",
            "count": 25,
            "enabled": true,
            "event": "release",
            "schedule": "0 */6 * * *",
            "filter": "title contains \"release\"",
            "todoist": { "connection": chosen, "project": "2203306141" }
        }))
    });

    let onchange = {
        let config = config.clone();
        Callback::from(move |value: serde_json::Value| config.set(Rc::new(value)))
    };

    // One descriptor covering every field kind, which is the thing worth
    // reviewing: a kind that renders nothing is invisible in any real workflow
    // type that happens not to use it.
    let fields = vec![
        FieldDescriptor::new(
            "name",
            "Text",
            FieldKind::Text {
                placeholder: Some("A name for this".to_string()),
            },
        )
        .required(),
        FieldDescriptor::new(
            "feed.url",
            "URL",
            FieldKind::Url {
                placeholder: Some("https://example.com/feed.xml".to_string()),
            },
        )
        .with_help("A dotted name writes into a nested object."),
        FieldDescriptor::new(
            "notes",
            "Text area",
            FieldKind::TextArea {
                placeholder: Some("What is this for?".to_string()),
            },
        ),
        FieldDescriptor::new(
            "secret",
            "Secret",
            FieldKind::Secret {
                placeholder: Some("a long random string".to_string()),
                generator: true,
                generator_bytes: 32,
            },
        )
        .with_help("Generated in the browser, and revealable so it can be checked."),
        FieldDescriptor::new(
            "count",
            "Number",
            FieldKind::Number {
                min: Some(1.0),
                max: Some(100.0),
                step: Some(1.0),
            },
        ),
        FieldDescriptor::new("enabled", "Boolean", FieldKind::Boolean),
        FieldDescriptor::new(
            "event",
            "Select",
            FieldKind::Select {
                options: vec![
                    OptionItem::new("push", "Push"),
                    OptionItem::new("pull_request", "Pull request"),
                    OptionItem::new("release", "Release"),
                ],
            },
        ),
        FieldDescriptor::new(
            "todoist.connection",
            "Connection",
            FieldKind::Connection {
                provider: "todoist".to_string(),
                connection_kind: None,
            },
        )
        .required(),
        FieldDescriptor::new(
            "todoist.project",
            "Options",
            FieldKind::Options {
                source: "projects".to_string(),
                depends_on: "todoist.connection".to_string(),
                parent: None,
            },
        ),
        FieldDescriptor::new("schedule", "Cron", FieldKind::Cron),
        FieldDescriptor::new(
            "filter",
            "Filter",
            FieldKind::Filter {
                fields: vec!["title".to_string(), "author".to_string()],
            },
        ),
    ];

    let options = HashMap::from([(
        "todoist.project".to_string(),
        fixtures::connection_options("projects", None),
    )]);

    let errors = HashMap::from([
        (
            "name".to_string(),
            "Another workflow already uses that name.".to_string(),
        ),
        (
            "feed.url".to_string(),
            "That address did not respond with a feed.".to_string(),
        ),
    ]);

    html! {
        <>
            <Specimen label="Every field kind">
                <DynamicForm
                    fields={fields.clone()}
                    config={(*config).clone()}
                    onchange={onchange}
                    connections={connections.clone()}
                    options={options.clone()}
                />
            </Specimen>

            <Specimen
                label="With errors the agent reported, while saving"
                note="Disabled for the duration of the request, so a slow save cannot be \
                      submitted twice or edited underneath."
            >
                <DynamicForm
                    {fields}
                    config={(*config).clone()}
                    onchange={inert()}
                    {connections}
                    {options}
                    {errors}
                    disabled=true
                />
            </Specimen>
        </>
    }
}

#[function_component(Documentations)]
fn documentations() -> Html {
    let markdown = fixtures::workflow_types()
        .into_iter()
        .find(|descriptor| !descriptor.documentation.is_empty())
        .map(|descriptor| descriptor.documentation)
        .unwrap_or_default();

    html! {
        <>
            <Specimen
                label="Collapsed by default"
                note="Somebody adding their fourth feed does not need to scroll past an \
                      explanation of RSS to reach the form."
            >
                <Documentation markdown={markdown} />
            </Specimen>

            <Specimen
                label="Nothing to say"
                note="A type with no guidance renders no control at all, rather than an empty one."
            >
                <Documentation markdown="" />
            </Specimen>
        </>
    }
}

#[function_component(WebhookAddresses)]
fn webhook_addresses() -> Html {
    // Rotating really does reissue the address, because the demo store is
    // standing in for the agent — which is the only way to see that the field
    // updates rather than merely that the button is clickable.
    let reload = use_state(|| 0u32);
    let workflow = fixtures::workflows()
        .into_iter()
        .find(|workflow| workflow.webhook_path.is_some());

    let on_rotated = {
        let reload = reload.clone();
        Callback::from(move |_| reload.set(*reload + 1))
    };

    let Some(workflow) = workflow else {
        return html! { <Specimen label="No webhook workflow in the fixtures" /> };
    };

    html! {
        <Specimen
            label="Issued"
            note="The address is completed from the browser's own origin, because an agent \
                  behind a proxy does not reliably know the one it is reached on."
        >
            <WebhookAddress
                key={*reload}
                workflow={workflow.id.to_string()}
                path={workflow.webhook_path.clone().unwrap_or_default()}
                {on_rotated}
            />
        </Specimen>
    }
}

#[function_component(Toolbars)]
fn toolbars() -> Html {
    html! {
        <>
            <Specimen label="Refresh">
                <div class="specimen__row">
                    <RefreshButton onclick={inert()} />
                    <RefreshButton onclick={inert()} busy=true />
                </div>
            </Specimen>

            <Specimen
                label="Connect"
                note="Lists the integrations the agent has configured, and renders nothing when \
                      it has none. Starting a setup needs a real agent, so it reports that here."
            >
                <ConnectMenu />
            </Specimen>
        </>
    }
}

#[function_component(ConnectionsPanels)]
fn connections_panels() -> Html {
    html! {
        <Specimen
            label="Loaded"
            note="One integration is connected and one is not, so both states are on screen. \
                  Disconnecting is confirmed first, and sticks for the rest of the session."
        >
            <ConnectionsPanel />
        </Specimen>
    }
}
