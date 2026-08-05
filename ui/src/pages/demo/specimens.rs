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

use crate::components::{
    Alert, AlertKind, BrowserEntry, BrowserPartition, Button, ButtonGroup, ButtonKind, ConnectMenu,
    ConnectionsPanel, DbEntity, Documentation, DynamicForm, EntityMetadata, Field, FilterInput,
    JsonHighlight, NumberInput, PageTitle, PartitionBrowser, RefreshButton, SecretInput, Select,
    SelectOption, Switch, TextArea, TextInput, WebhookAddress,
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

/// Every control the gallery covers, in the order it lists them: the primitives
/// a page is assembled from first, then the composites built out of those.
pub const CONTROLS: &[Control] = &[
    Control {
        slug: "alert",
        name: "Alert",
        blurb: "A page-level notice, and where a page offers a way out of a failure.",
        view: || html! { <Alerts /> },
    },
    Control {
        slug: "button",
        name: "Button",
        blurb: "Every prominence and state a button can be in.",
        view: || html! { <Buttons /> },
    },
    Control {
        slug: "field",
        name: "Field",
        blurb: "The label, help text and error that wrap every control.",
        view: || html! { <Fields /> },
    },
    Control {
        slug: "text-input",
        name: "Text input",
        blurb: "A single line of text, including the shape used for pasted tokens.",
        view: || html! { <TextInputs /> },
    },
    Control {
        slug: "text-area",
        name: "Text area",
        blurb: "Several lines of text, in prose and monospaced forms.",
        view: || html! { <TextAreas /> },
    },
    Control {
        slug: "secret-input",
        name: "Secret input",
        blurb: "A masked token, revealable, and optionally generated in the browser.",
        view: || html! { <SecretInputs /> },
    },
    Control {
        slug: "number-input",
        name: "Number input",
        blurb: "A number, where empty and zero are different answers.",
        view: || html! { <NumberInputs /> },
    },
    Control {
        slug: "select",
        name: "Select",
        blurb: "A single choice, including the case where the stored value is no longer offered.",
        view: || html! { <Selects /> },
    },
    Control {
        slug: "switch",
        name: "Switch",
        blurb: "A yes or no, taking effect as soon as it is flipped.",
        view: || html! { <Switches /> },
    },
    Control {
        slug: "filter-input",
        name: "Filter input",
        blurb: "A filter expression, with the diagnostics it offers as you type.",
        view: || html! { <FilterInputs /> },
    },
    Control {
        slug: "json-highlight",
        name: "JSON",
        blurb: "A stored payload, rendered as escaped text rather than markup.",
        view: || html! { <JsonHighlights /> },
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
        slug: "page-title",
        name: "Page title",
        blurb: "The heading row, with and without the actions a page injects.",
        view: || html! { <PageTitles /> },
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

#[function_component(Alerts)]
fn alerts() -> Html {
    let dismissed = use_state(|| false);

    let on_close = {
        let dismissed = dismissed.clone();
        Callback::from(move |_| dismissed.set(true))
    };
    let restore = {
        let dismissed = dismissed.clone();
        Callback::from(move |_: MouseEvent| dismissed.set(false))
    };

    html! {
        <>
            <Specimen label="Error">
                <Alert
                    kind={AlertKind::Error}
                    title="We could not load your workflows."
                    message="Network error: failed to fetch"
                />
            </Specimen>

            <Specimen label="Warning">
                <Alert
                    kind={AlertKind::Warning}
                    title="This connection needs reconnecting"
                    message="Spotify rejected the stored credential. Workflows that publish \
                             through it are paused until it is authorised again."
                />
            </Specimen>

            <Specimen label="Info">
                <Alert
                    kind={AlertKind::Info}
                    title="Nothing has run yet"
                    message="This workflow is enabled but its schedule has not come round."
                />
            </Specimen>

            <Specimen label="Success">
                <Alert kind={AlertKind::Success} title="Your changes were saved." />
            </Specimen>

            <Specimen
                label="With actions"
                note="A recovery action turns a dead end into something the reader can do."
            >
                <Alert
                    kind={AlertKind::Error}
                    title="Your session has expired"
                    message="Sign in again to inspect the admin stores."
                >
                    <Button kind={ButtonKind::Primary} small=true onclick={inert()}>
                        { "Sign in" }
                    </Button>
                    <Button kind={ButtonKind::Subtle} small=true onclick={inert()}>
                        { "Reload" }
                    </Button>
                </Alert>
            </Specimen>

            <Specimen
                label="Dismissible"
                note="Only alerts the reader can safely ignore get a dismiss affordance."
            >
                if *dismissed {
                    <Button kind={ButtonKind::Subtle} small=true onclick={restore}>
                        { "Bring it back" }
                    </Button>
                } else {
                    <Alert
                        kind={AlertKind::Info}
                        title="Demo mode is active"
                        message="Everything on this page is served from fixtures."
                        on_close={on_close}
                    />
                }
            </Specimen>
        </>
    }
}

#[function_component(Buttons)]
fn buttons() -> Html {
    let row = |label: &'static str, kind: ButtonKind| {
        html! {
            <Specimen label={label}>
                <div class="specimen__row">
                    <Button {kind} onclick={inert()}>{ "Save" }</Button>
                    <Button {kind} onclick={inert()} disabled=true>{ "Disabled" }</Button>
                    <Button {kind} onclick={inert()} busy=true>{ "Saving" }</Button>
                    <Button {kind} onclick={inert()} small=true>{ "Small" }</Button>
                </div>
            </Specimen>
        }
    };

    html! {
        <>
            { row("Default", ButtonKind::Default) }
            { row("Primary", ButtonKind::Primary) }
            { row("Danger", ButtonKind::Danger) }
            { row("Subtle", ButtonKind::Subtle) }

            <Specimen
                label="Grouped"
                note="Actions on one thing, joined so they share an edge instead of a gap. The \
                      outer corners are the only rounded ones, whichever buttons happen to be \
                      shown."
            >
                <div class="specimen__row">
                    <ButtonGroup label="Workflow actions">
                        <Button onclick={inert()}>{ "Edit" }</Button>
                        <Button onclick={inert()}>{ "Trigger" }</Button>
                        <Button kind={ButtonKind::Danger} onclick={inert()}>{ "Delete" }</Button>
                    </ButtonGroup>

                    <ButtonGroup label="Workflow actions">
                        <Button onclick={inert()} disabled=true>{ "Edit" }</Button>
                        <Button kind={ButtonKind::Danger} onclick={inert()} busy=true>
                            { "Delete" }
                        </Button>
                    </ButtonGroup>

                    <ButtonGroup label="Workflow actions">
                        <Button kind={ButtonKind::Danger} onclick={inert()}>{ "Delete" }</Button>
                    </ButtonGroup>
                </div>
            </Specimen>
        </>
    }
}

#[function_component(Fields)]
fn fields() -> Html {
    let value = use_state(|| "https://example.com/feed.xml".to_string());

    html! {
        <>
            <Specimen label="Plain">
                <Field id="demo-plain" label="Feed address">
                    <TextInput id="demo-plain" value={(*value).clone()} onchange={bind(&value)} />
                </Field>
            </Specimen>

            <Specimen label="Required, with help">
                <Field
                    id="demo-help"
                    label="Feed address"
                    required=true
                    help="The RSS or Atom address to poll."
                >
                    <TextInput id="demo-help" value={(*value).clone()} onchange={bind(&value)} />
                </Field>
            </Specimen>

            <Specimen
                label="Refused by the agent"
                note="The error replaces the help text rather than stacking beneath it, so the \
                      fields below do not move as the reader types."
            >
                <Field
                    id="demo-error"
                    label="Feed address"
                    required=true
                    help="The RSS or Atom address to poll."
                    error="That address did not respond with a feed."
                >
                    <TextInput
                        id="demo-error"
                        value={(*value).clone()}
                        onchange={bind(&value)}
                        invalid=true
                    />
                </Field>
            </Specimen>
        </>
    }
}

#[function_component(TextInputs)]
fn text_inputs() -> Html {
    let empty = use_state(String::new);
    let filled = use_state(|| "https://blog.sierrasoftworks.com/feed.xml".to_string());
    let secret = use_state(|| "0123456789abcdef0123456789abcdef".to_string());
    let invalid = use_state(|| "not an address".to_string());

    html! {
        <>
            <Specimen label="Empty, with a placeholder">
                <TextInput
                    id="demo-text-empty"
                    value={(*empty).clone()}
                    onchange={bind(&empty)}
                    placeholder="https://example.com/feed.xml"
                />
            </Specimen>

            <Specimen label="Filled">
                <TextInput
                    id="demo-text-filled"
                    value={(*filled).clone()}
                    onchange={bind(&filled)}
                />
            </Specimen>

            <Specimen
                label="Secret"
                note="Only protects what is on screen while it is typed; a stored token is never \
                      sent back to be shown."
            >
                <TextInput
                    id="demo-text-secret"
                    value={(*secret).clone()}
                    onchange={bind(&secret)}
                    secret=true
                />
            </Specimen>

            <Specimen label="Invalid">
                <TextInput
                    id="demo-text-invalid"
                    value={(*invalid).clone()}
                    onchange={bind(&invalid)}
                    invalid=true
                />
            </Specimen>

            <Specimen label="Disabled">
                <TextInput
                    id="demo-text-disabled"
                    value={(*filled).clone()}
                    onchange={inert()}
                    disabled=true
                />
            </Specimen>
        </>
    }
}

#[function_component(TextAreas)]
fn text_areas() -> Html {
    let prose = use_state(|| {
        "Announces new releases in the team channel.\nDisabled while the release process is \
         being reworked."
            .to_string()
    });
    let code = use_state(|| "title contains \"release\"\n    and not draft".to_string());

    html! {
        <>
            <Specimen label="Prose">
                <TextArea id="demo-area" value={(*prose).clone()} onchange={bind(&prose)} />
            </Specimen>

            <Specimen
                label="Monospaced"
                note="For values whose alignment carries meaning, such as an expression."
            >
                <TextArea
                    id="demo-area-mono"
                    value={(*code).clone()}
                    onchange={bind(&code)}
                    monospace=true
                    rows={4}
                />
            </Specimen>

            <Specimen label="Empty, invalid, and disabled">
                <div class="specimen__stack">
                    <TextArea
                        id="demo-area-empty"
                        value=""
                        onchange={inert()}
                        placeholder="What is this for?"
                    />
                    <TextArea
                        id="demo-area-invalid"
                        value={(*code).clone()}
                        onchange={bind(&code)}
                        monospace=true
                        invalid=true
                    />
                    <TextArea
                        id="demo-area-disabled"
                        value={(*prose).clone()}
                        onchange={inert()}
                        disabled=true
                    />
                </div>
            </Specimen>
        </>
    }
}

#[function_component(SecretInputs)]
fn secret_inputs() -> Html {
    let empty = use_state(String::new);
    let agreed = use_state(|| "s3cr3t-t0k3n-from-the-provider".to_string());
    let generated = use_state(String::new);

    html! {
        <>
            <Specimen
                label="Empty"
                note="Nothing to unmask yet, so the button that would do it is not drawn — but \
                      the room it takes is still reserved."
            >
                <SecretInput
                    id="demo-secret-empty"
                    value={(*empty).clone()}
                    onchange={bind(&empty)}
                    placeholder="a long random string"
                />
            </Specimen>

            <Specimen
                label="Filled"
                note="Revealable, because the whole job is to make this string match one on the \
                      provider's own settings page, and nobody can check that against dots."
            >
                <SecretInput
                    id="demo-secret-filled"
                    value={(*agreed).clone()}
                    onchange={bind(&agreed)}
                />
            </Specimen>

            <Specimen
                label="With a generator"
                note="For the secrets both sides merely agree on. Generated here rather than by \
                      the agent, so a fresh one never travels over the wire to be shown."
            >
                <SecretInput
                    id="demo-secret-generated"
                    value={(*generated).clone()}
                    onchange={bind(&generated)}
                    placeholder="a long random string"
                    generator=true
                />
            </Specimen>

            <Specimen label="Invalid and disabled">
                <div class="specimen__stack">
                    <SecretInput
                        id="demo-secret-invalid"
                        value={(*agreed).clone()}
                        onchange={inert()}
                        generator=true
                        invalid=true
                    />
                    <SecretInput
                        id="demo-secret-disabled"
                        value={(*agreed).clone()}
                        onchange={inert()}
                        generator=true
                        disabled=true
                    />
                </div>
            </Specimen>
        </>
    }
}

#[function_component(NumberInputs)]
fn number_inputs() -> Html {
    let unset = use_state(|| None::<i64>);
    let bounded = use_state(|| Some(2i64));

    let on_unset = {
        let unset = unset.clone();
        Callback::from(move |value: Option<i64>| unset.set(value))
    };
    let on_bounded = {
        let bounded = bounded.clone();
        Callback::from(move |value: Option<i64>| bounded.set(value))
    };

    html! {
        <>
            <Specimen
                label="Unset"
                note="An empty box means the field is unset, which is a different answer from zero."
            >
                <NumberInput
                    id="demo-number-unset"
                    value={*unset}
                    onchange={on_unset}
                    placeholder="No limit"
                />
            </Specimen>

            <Specimen label="Bounded">
                <NumberInput
                    id="demo-number-bounded"
                    value={*bounded}
                    onchange={on_bounded}
                    min={1}
                    max={4}
                />
            </Specimen>

            <Specimen label="Invalid and disabled">
                <div class="specimen__row">
                    <NumberInput
                        id="demo-number-invalid"
                        value={Some(9i64)}
                        onchange={inert()}
                        min={1}
                        max={4}
                        invalid=true
                    />
                    <NumberInput
                        id="demo-number-disabled"
                        value={Some(2i64)}
                        onchange={inert()}
                        disabled=true
                    />
                </div>
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
                <Select
                    id="demo-select-empty"
                    value={(*chosen).clone().map(AttrValue::from)}
                    onchange={on_chosen}
                    options={options.clone()}
                    placeholder="Choose a project"
                />
            </Specimen>

            <Specimen label="Clearable">
                <Select
                    id="demo-select-clearable"
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
                <Select
                    id="demo-select-missing"
                    value={Some(AttrValue::from("2203306999"))}
                    onchange={inert()}
                    options={options.clone()}
                />
            </Specimen>

            <Specimen label="Invalid and disabled">
                <div class="specimen__row">
                    <Select
                        id="demo-select-invalid"
                        value={None}
                        onchange={inert()}
                        options={options.clone()}
                        invalid=true
                    />
                    <Select
                        id="demo-select-disabled"
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

#[function_component(Switches)]
fn switches() -> Html {
    let on = use_state(|| true);
    let off = use_state(|| false);

    let toggle = |state: &UseStateHandle<bool>| {
        let state = state.clone();
        Callback::from(move |value: bool| state.set(value))
    };

    html! {
        <>
            <Specimen label="Unlabelled">
                <div class="specimen__row">
                    <Switch id="demo-switch-off" checked={*off} onchange={toggle(&off)} />
                    <Switch id="demo-switch-on" checked={*on} onchange={toggle(&on)} />
                </div>
            </Specimen>

            <Specimen label="Labelled" note="A bare toggle says nothing about what it controls.">
                <Switch
                    id="demo-switch-labelled"
                    checked={*on}
                    onchange={toggle(&on)}
                    label="Include the summary"
                />
            </Specimen>

            <Specimen label="Disabled">
                <div class="specimen__row">
                    <Switch
                        id="demo-switch-disabled-off"
                        checked={false}
                        onchange={inert()}
                        label="Off"
                        disabled=true
                    />
                    <Switch
                        id="demo-switch-disabled-on"
                        checked={true}
                        onchange={inert()}
                        label="On"
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
                    id="demo-filter-empty"
                    value={(*empty).clone()}
                    onchange={bind(&empty)}
                    fields={fields.clone()}
                />
            </Specimen>

            <Specimen label="Valid">
                <FilterInput
                    id="demo-filter-valid"
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
                    id="demo-filter-unknown"
                    value={(*unknown).clone()}
                    onchange={bind(&unknown)}
                    fields={fields.clone()}
                />
            </Specimen>

            <Specimen label="Does not parse">
                <FilterInput
                    id="demo-filter-broken"
                    value={(*broken).clone()}
                    onchange={bind(&broken)}
                    fields={fields.clone()}
                />
            </Specimen>

            <Specimen label="Refused by the agent, and disabled">
                <div class="specimen__stack">
                    <FilterInput
                        id="demo-filter-invalid"
                        value={(*valid).clone()}
                        onchange={bind(&valid)}
                        fields={fields.clone()}
                        invalid=true
                    />
                    <FilterInput
                        id="demo-filter-disabled"
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

#[function_component(JsonHighlights)]
fn json_highlights() -> Html {
    html! {
        <>
            <Specimen label="A nested payload">
                <JsonHighlight value={json!({
                    "issuer": "https://accounts.google.com",
                    "scopes": ["openid", "email", "profile"],
                    "expires_in": 3600,
                    "refreshable": true,
                    "revoked_at": null
                })} />
            </Specimen>

            <Specimen
                label="A payload containing markup"
                note="Every token is emitted through Yew's interpolation, which escapes it, so a \
                      stored payload cannot inject markup."
            >
                <JsonHighlight value={json!({
                    "title": "<script>alert('xss')</script>",
                    "url": "https://example.com/?a=1&b=2"
                })} />
            </Specimen>

            <Specimen label="Empty">
                <JsonHighlight value={json!({})} />
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
                    <Button kind={ButtonKind::Subtle} small=true onclick={inert()}>{ "Trigger" }</Button>
                    <Button kind={ButtonKind::Danger} small=true onclick={inert()}>{ "Delete" }</Button>
                </DbEntity>
            </Specimen>
        </>
    }
}

#[function_component(Browsers)]
fn browsers() -> Html {
    let partition = |name: &'static str, kind: &'static str, entries: Vec<BrowserEntry>| {
        BrowserPartition {
            id: AttrValue::from(format!("{kind}:{name}")),
            name: AttrValue::from(name),
            kind: AttrValue::from(kind),
            icon: html! {
                <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor"
                    stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <ellipse cx="12" cy="5" rx="9" ry="3" />
                    <path d="M3 5v14a9 3 0 0 0 18 0V5" />
                    <path d="M3 12a9 3 0 0 0 18 0" />
                </svg>
            },
            entries,
        }
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

#[function_component(PageTitles)]
fn page_titles() -> Html {
    html! {
        <>
            <Specimen label="Title only">
                <PageTitle title="Workflows" />
            </Specimen>

            <Specimen label="With a subtitle">
                <PageTitle
                    title="Workflows"
                    subtitle="The things Automate watches for you, and what it does when they change."
                />
            </Specimen>

            <Specimen label="With the actions a page injects">
                <PageTitle
                    title="Admin"
                    subtitle="Browse the key-value store and job queues across every partition."
                >
                    <div class="page-title__actions">
                        <RefreshButton onclick={inert()} />
                    </div>
                </PageTitle>
            </Specimen>
        </>
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
