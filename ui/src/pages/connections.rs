//! Managing the services this account has linked.
//!
//! Credentials travel in one direction only. A token can be entered here and is
//! sent to the agent, but nothing sends one back — the list is built from
//! summaries which have nowhere to carry one — so a credential cannot be
//! recovered from this page once it has been saved, only replaced.

use automate_api::{ConnectionKind, ConnectionStatus, ConnectionSummary};
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind, Button, ButtonKind, Field, Select, SelectOption, TextInput};
use crate::util::short_relative;

/// The services that can be linked by pasting in a token.
///
/// Services authorised through OAuth are absent because there is nothing for
/// somebody to paste: the setup wizard obtains the credential itself.
const PASTEABLE_PROVIDERS: &[(&str, &str)] = &[
    ("todoist", "Todoist"),
    ("github", "GitHub"),
    ("ynab", "YNAB"),
];

/// Where to find the token for each service, so nobody has to go hunting.
fn where_to_find_the_token(provider: &str) -> &'static str {
    match provider {
        "todoist" => "Todoist → Settings → Integrations → Developer → API token.",
        "github" => "GitHub → Settings → Developer settings → Personal access tokens.",
        "ynab" => "YNAB → Account Settings → Developer Settings → Personal Access Tokens.",
        _ => "Look for API tokens in the service's account or developer settings.",
    }
}

#[function_component(Connections)]
pub fn connections() -> Html {
    let connections = use_state(|| None::<Vec<ConnectionSummary>>);
    let error = use_state(|| None::<String>);
    let reload = use_state(|| 0u32);

    {
        let connections = connections.clone();
        let error = error.clone();

        use_effect_with(*reload, move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                match api::list_service_connections().await {
                    Ok(loaded) => {
                        connections.set(Some(loaded));
                        error.set(None);
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
            });
        });
    }

    let on_changed = {
        let reload = reload.clone();
        Callback::from(move |_| reload.set(*reload + 1))
    };

    html! {
        <section class="connections">
            <p class="connections__intro">
                { "Workflows publish through the services you link here. Credentials are \
                   encrypted before they are stored and are never shown again once saved." }
            </p>

            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not load your connections."
                    message={message.clone()}
                />
            }

            <LinkService on_linked={on_changed.clone()} />

            {
                match &*connections {
                    None => html! { <p class="connections__empty">{ "Loading…" }</p> },
                    Some(list) if list.is_empty() => html! {
                        <p class="connections__empty">
                            { "You have not linked any services yet." }
                        </p>
                    },
                    Some(list) => html! {
                        <ul class="connections__list">
                            { for list.iter().map(|connection| html! {
                                <li key={connection.id.to_string()}>
                                    <ConnectionRow
                                        connection={connection.clone()}
                                        on_changed={on_changed.clone()}
                                    />
                                </li>
                            }) }
                        </ul>
                    },
                }
            }
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct LinkServiceProps {
    on_linked: Callback<()>,
}

/// The form for linking a service with a token the user pastes in.
#[function_component(LinkService)]
fn link_service(props: &LinkServiceProps) -> Html {
    let provider = use_state(|| None::<String>);
    let name = use_state(String::new);
    let key = use_state(String::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let onsubmit = {
        let (provider, name, key) = (provider.clone(), name.clone(), key.clone());
        let (busy, error) = (busy.clone(), error.clone());
        let on_linked = props.on_linked.clone();

        Callback::from(move |_: MouseEvent| {
            let Some(chosen) = (*provider).clone() else {
                error.set(Some("Choose which service to connect.".into()));
                return;
            };

            if key.trim().is_empty() {
                error.set(Some("Paste the token this service issued you.".into()));
                return;
            }

            // Defaulting the name to the service is enough until somebody links
            // a second account on the same one.
            let label = if name.trim().is_empty() {
                PASTEABLE_PROVIDERS
                    .iter()
                    .find(|(id, _)| *id == chosen)
                    .map(|(_, label)| (*label).to_string())
                    .unwrap_or_else(|| chosen.clone())
            } else {
                name.trim().to_string()
            };

            let (provider, name, key) = (provider.clone(), name.clone(), key.clone());
            let (busy, error) = (busy.clone(), error.clone());
            let on_linked = on_linked.clone();
            let token = (*key).clone();

            busy.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                match api::create_service_connection(&chosen, &label, &token).await {
                    Ok(_) => {
                        provider.set(None);
                        name.set(String::new());
                        key.set(String::new());
                        error.set(None);
                        on_linked.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let options: Vec<SelectOption> = PASTEABLE_PROVIDERS
        .iter()
        .map(|(id, label)| SelectOption::new(*id, *label))
        .collect();

    let on_provider = {
        let provider = provider.clone();
        Callback::from(move |value: Option<String>| provider.set(value))
    };
    let on_name = {
        let name = name.clone();
        Callback::from(move |value: String| name.set(value))
    };
    let on_key = {
        let key = key.clone();
        Callback::from(move |value: String| key.set(value))
    };

    html! {
        <div class="connections__form">
            <h3 class="connections__form-title">{ "Link a service" }</h3>

            if let Some(message) = &*error {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not link that service."
                    message={message.clone()}
                />
            }

            <Field id="connection-provider" label="Service" required=true>
                <Select
                    id="connection-provider"
                    value={(*provider).clone().map(AttrValue::from)}
                    onchange={on_provider}
                    {options}
                    placeholder="Choose a service"
                />
            </Field>

            <Field
                id="connection-name"
                label="Name"
                help="Optional. Useful once you have linked more than one account on the same service."
            >
                <TextInput
                    id="connection-name"
                    value={(*name).clone()}
                    onchange={on_name}
                    placeholder="Personal"
                />
            </Field>

            <Field
                id="connection-key"
                label="Token"
                required=true
                help={provider.as_ref().map(|p| AttrValue::from(where_to_find_the_token(p)))}
            >
                <TextInput
                    id="connection-key"
                    value={(*key).clone()}
                    onchange={on_key}
                    secret=true
                    placeholder="Paste the token here"
                />
            </Field>

            <Button kind={ButtonKind::Primary} onclick={onsubmit} busy={*busy}>
                { "Link service" }
            </Button>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct ConnectionRowProps {
    connection: ConnectionSummary,
    on_changed: Callback<()>,
}

#[function_component(ConnectionRow)]
fn connection_row(props: &ConnectionRowProps) -> Html {
    let busy = use_state(|| false);
    let connection = props.connection.clone();

    let onremove = {
        let id = connection.id.to_string();
        let name = connection.name.clone();
        let busy = busy.clone();
        let on_changed = props.on_changed.clone();

        Callback::from(move |_: MouseEvent| {
            // Unlinking stops every workflow that publishes through it, which is
            // not obvious from the button alone.
            let confirmed = web_sys::window()
                .and_then(|w| {
                    w.confirm_with_message(&format!(
                        "Unlink {name}? Workflows that publish through it will stop working."
                    ))
                    .ok()
                })
                .unwrap_or(false);

            if !confirmed {
                return;
            }

            let (id, busy, on_changed) = (id.clone(), busy.clone(), on_changed.clone());
            busy.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                let _ = api::delete_service_connection(&id).await;
                busy.set(false);
                on_changed.emit(());
            });
        })
    };

    html! {
        <div class="connection">
            <div class="connection__detail">
                <span class="connection__name">{ &connection.name }</span>
                <span class="connection__meta">
                    { &connection.provider }
                    { " · " }
                    { kind_label(connection.kind) }
                    { " · linked " }
                    { short_relative(connection.created_at) }
                </span>
            </div>

            <StatusBadge status={connection.status} />

            <Button
                kind={ButtonKind::Danger}
                small=true
                onclick={onremove}
                busy={*busy}
                title="Unlink this service"
            >
                { "Unlink" }
            </Button>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct StatusBadgeProps {
    status: ConnectionStatus,
}

#[function_component(StatusBadge)]
fn status_badge(props: &StatusBadgeProps) -> Html {
    // Only says something when there is something to say: a working connection
    // is the expected case, and labelling every row "OK" is noise that makes the
    // rows that do need attention harder to spot.
    let (class, label) = match props.status {
        ConnectionStatus::Ok => return html! {},
        ConnectionStatus::NeedsReauthorization => {
            ("connection__status--warning", "Needs reconnecting")
        }
        ConnectionStatus::Error => ("connection__status--error", "Not working"),
    };

    html! {
        <span class={classes!("connection__status", class)}>{ label }</span>
    }
}

fn kind_label(kind: ConnectionKind) -> &'static str {
    match kind {
        ConnectionKind::OAuth2 => "authorised",
        ConnectionKind::ApiKey => "token",
        ConnectionKind::GitHubApp => "app installation",
    }
}
