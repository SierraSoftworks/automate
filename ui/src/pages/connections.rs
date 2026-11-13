//! Managing the services this account has linked.
//!
//! Credentials travel in one direction only. A token can be entered here and is
//! sent to the agent, but nothing sends one back — the list is built from
//! summaries which have nowhere to carry one — so a credential cannot be
//! recovered from this page once it has been saved, only replaced.

use automate_api::{ConnectionKind, ConnectionStatus, ConnectionSummary};
use yew::prelude::*;

use crate::api;
use yewi::prelude::*;

use crate::components::{ConnectMenu, PageActions};
use crate::search::{MatchContext, SearchContext};

/// The services that can be linked by pasting in a token.
///
/// Services authorised through OAuth are absent because there is nothing for
/// somebody to paste: the setup wizard obtains the credential itself.
const PASTEABLE_PROVIDERS: &[(&str, &str)] = &[
    ("alphavantage", "AlphaVantage"),
    ("todoist", "Todoist"),
    ("github", "GitHub"),
    ("ynab", "YNAB"),
];

/// Where to find the token for each service, so nobody has to go hunting.
fn where_to_find_the_token(provider: &str) -> &'static str {
    match provider {
        "alphavantage" => "AlphaVantage → Support → Claim your free API key.",
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
    let chosen = use_state(|| None::<String>);

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

    let on_provider = {
        let chosen = chosen.clone();
        Callback::from(move |provider: String| chosen.set(Some(provider)))
    };

    let on_cancel_add = {
        let chosen = chosen.clone();
        Callback::from(move |_| chosen.set(None))
    };

    let on_linked = {
        let (chosen, reload) = (chosen.clone(), reload.clone());
        Callback::from(move |_| {
            chosen.set(None);
            reload.set(*reload + 1);
        })
    };

    let provider_options: Vec<(AttrValue, AttrValue)> = PASTEABLE_PROVIDERS
        .iter()
        .map(|(id, label)| (AttrValue::from(*id), AttrValue::from(*label)))
        .collect();

    let page_actions = use_context::<PageActions>();
    {
        let page_actions = page_actions.clone();
        let provider_options = provider_options.clone();
        let on_provider = on_provider.clone();
        let onstarted = on_changed.clone();
        let disabled = connections.is_none() || chosen.is_some();
        use_effect_with(disabled, move |_| {
            if let Some(actions) = &page_actions {
                actions.set(html! {
                    <div class="page-title__actions">
                        <ConnectMenu
                            label="Add Connection"
                            credential_options={provider_options}
                            oncredential={on_provider}
                            onstarted={onstarted}
                            variant={ButtonVariant::Primary}
                            {disabled}
                            size={Size::Medium}
                        />
                    </div>
                });
            }
            move || {
                if let Some(actions) = page_actions {
                    actions.clear();
                }
            }
        });
    }

    let search = use_context::<SearchContext>();
    let filter = search
        .as_ref()
        .map(|search| search.filter.clone())
        .unwrap_or_default();

    html! {
        <section class="connections-page">
            if let Some(message) = &*error {
                <Alert tone={Tone::Danger} title="We could not load your connections.">
                    { message.clone() }
                </Alert>
            }

            if let Some(provider) = (*chosen).clone() {
                <LinkService {provider} on_linked={on_linked} oncancel={on_cancel_add} />
            }

            {
                match &*connections {
                    None => html! { <Loading label="Loading…" layout={LoadingLayout::Block} /> },
                    Some(list) if list.is_empty() => html! {
                        <EmptyState title="You have not linked any services yet." icon={icons::LINK}>
                            { "Add a connection to give your workflows somewhere to read from and publish to." }
                        </EmptyState>
                    },
                    Some(list) => {
                        let visible: Vec<&ConnectionSummary> = list
                            .iter()
                            .filter(|connection| {
                                let text = format!(
                                    "{} {} {} {} {}",
                                    connection.name,
                                    connection.provider,
                                    connection.kind.as_str(),
                                    connection.account.as_deref().unwrap_or_default(),
                                    connection.status.as_str(),
                                )
                                .to_lowercase();
                                filter.matches(&MatchContext {
                                    fields: &[
                                        ("name", &connection.name),
                                        ("service", &connection.provider),
                                        ("kind", connection.kind.as_str()),
                                        ("status", connection.status.as_str()),
                                    ],
                                    text: &text,
                                })
                            })
                            .collect();

                        if visible.is_empty() {
                            html! { <EmptyState title="No connections match your search." icon={icons::SEARCH_X} /> }
                        } else {
                            html! {
                                <ul class="connections__list">
                                    { for visible.into_iter().map(|connection| html! {
                                        <li key={connection.id.to_string()}>
                                            <ConnectionRow
                                                connection={connection.clone()}
                                                on_changed={on_changed.clone()}
                                            />
                                        </li>
                                    }) }
                                </ul>
                            }
                        }
                    },
                }
            }
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct LinkServiceProps {
    provider: String,
    on_linked: Callback<()>,
    oncancel: Callback<()>,
}

/// The form for linking a service with a token the user pastes in.
#[function_component(LinkService)]
fn link_service(props: &LinkServiceProps) -> Html {
    let name = use_state(String::new);
    let key = use_state(String::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let onsubmit = {
        let (provider, name, key) = (props.provider.clone(), name.clone(), key.clone());
        let (busy, error) = (busy.clone(), error.clone());
        let on_linked = props.on_linked.clone();

        Callback::from(move |_: MouseEvent| {
            if key.trim().is_empty() {
                error.set(Some("Paste the token this service issued you.".into()));
                return;
            }

            // Defaulting the name to the service is enough until somebody links
            // a second account on the same one.
            let label = if name.trim().is_empty() {
                PASTEABLE_PROVIDERS
                    .iter()
                    .find(|(id, _)| *id == provider)
                    .map(|(_, label)| (*label).to_string())
                    .unwrap_or_else(|| provider.clone())
            } else {
                name.trim().to_string()
            };

            let (provider, key) = (provider.clone(), key.clone());
            let (busy, error) = (busy.clone(), error.clone());
            let on_linked = on_linked.clone();
            let token = (*key).clone();

            busy.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                match api::create_service_connection(&provider, &label, &token).await {
                    Ok(_) => {
                        on_linked.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let on_name = {
        let name = name.clone();
        Callback::from(move |value: String| name.set(value))
    };
    let on_key = {
        let key = key.clone();
        Callback::from(move |value: String| key.set(value))
    };
    let on_cancel = {
        let oncancel = props.oncancel.clone();
        Callback::from(move |_: MouseEvent| oncancel.emit(()))
    };

    let provider_label = PASTEABLE_PROVIDERS
        .iter()
        .find(|(id, _)| *id == props.provider)
        .map(|(_, label)| *label)
        .unwrap_or(props.provider.as_str());

    let footer = html! {
        <>
            <Button variant={ButtonVariant::Subtle} on_click={on_cancel} disabled={*busy}>
                { "Cancel" }
            </Button>
            <Button variant={ButtonVariant::Primary} on_click={onsubmit} loading={*busy}>
                { "Link service" }
            </Button>
        </>
    };

    html! {
        <Dialog
            open=true
            title={format!("Add {provider_label} connection")}
            {footer}
            on_close={props.oncancel.clone()}
        >
            <Stack>
                if let Some(message) = &*error {
                    <Alert tone={Tone::Danger} title="We could not link that service.">
                        { message.clone() }
                    </Alert>
                }
                <Field
                    label="Name"
                    hint="Optional. Useful once you have linked more than one account on the same service."
                >
                    <TextInput value={(*name).clone()} on_input={on_name} placeholder="Personal" />
                </Field>
                <Field
                    label="Token"
                    required=true
                    hint={where_to_find_the_token(&props.provider)}
                >
                    <SecretInput
                        value={(*key).clone()}
                        on_input={on_key}
                        placeholder="Paste the token here"
                    />
                </Field>
            </Stack>
        </Dialog>
    }
}

#[derive(Properties, PartialEq)]
struct ConnectionRowProps {
    connection: ConnectionSummary,
    on_changed: Callback<()>,
}

#[function_component(ConnectionRow)]
fn connection_row(props: &ConnectionRowProps) -> Html {
    let reconnecting = use_state(|| false);
    let removing = use_state(|| false);
    let confirming_remove = use_state(|| false);
    let editing = use_state(|| false);
    let reconnect_error = use_state(|| None::<String>);
    let connection = props.connection.clone();

    let onedit = {
        let editing = editing.clone();
        Callback::from(move |_: MouseEvent| editing.set(true))
    };

    let oncancel_edit = {
        let editing = editing.clone();
        Callback::from(move |_: ()| editing.set(false))
    };

    let onsaved = {
        let editing = editing.clone();
        let on_changed = props.on_changed.clone();
        Callback::from(move |_: ()| {
            editing.set(false);
            on_changed.emit(());
        })
    };

    let onreconnect = {
        let provider = connection.provider.clone();
        let reconnecting = reconnecting.clone();
        let reconnect_error = reconnect_error.clone();
        let on_changed = props.on_changed.clone();

        Callback::from(move |_: MouseEvent| {
            let provider = provider.clone();
            let reconnecting = reconnecting.clone();
            let reconnect_error = reconnect_error.clone();
            let on_changed = on_changed.clone();
            reconnecting.set(true);
            reconnect_error.set(None);

            wasm_bindgen_futures::spawn_local(async move {
                match api::start_setup(&provider).await {
                    Ok(url) => {
                        if let Some(window) = web_sys::window() {
                            let _ = window.open_with_url_and_target_and_features(
                                &url,
                                "automate-connect",
                                "popup,width=480,height=720",
                            );
                        }
                        on_changed.emit(());
                    }
                    Err(err) => reconnect_error.set(Some(err.to_string())),
                }

                reconnecting.set(false);
            });
        })
    };

    let onask_remove = {
        let confirming_remove = confirming_remove.clone();
        Callback::from(move |_: MouseEvent| confirming_remove.set(true))
    };
    let oncancel_remove = {
        let confirming_remove = confirming_remove.clone();
        Callback::from(move |_: ()| confirming_remove.set(false))
    };
    let onremove = {
        let id = connection.id.to_string();
        let removing = removing.clone();
        let confirming_remove = confirming_remove.clone();
        let on_changed = props.on_changed.clone();
        Callback::from(move |_: ()| {
            let (id, removing, on_changed) = (id.clone(), removing.clone(), on_changed.clone());
            let confirming_remove = confirming_remove.clone();
            removing.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                let _ = api::delete_service_connection(&id).await;
                removing.set(false);
                confirming_remove.set(false);
                on_changed.emit(());
            });
        })
    };

    html! {
        <div class="connection">
            <div class="connection__detail">
                <div class="connection__title">
                    <span class="connection__identity">
                        <span class="connection__provider">{ &connection.provider }{ "/" }</span>
                        <span class="connection__name">{ &connection.name }</span>
                    </span>
                    if let Some(account_type) = account_type(&connection) {
                        <Tag class="connection__account-type">{ account_type }</Tag>
                    }
                </div>
                <Text size={TextSize::Sm} color={TextColor::Muted} class="connection__meta">
                    { kind_label(connection.kind) }
                    { " · linked " }
                    <Timestamp at={connection.created_at} compact=true />
                </Text>
            </div>
            <StatusBadge status={connection.status} />
            <ButtonGroup label="Connection actions">
                if connection.status == ConnectionStatus::NeedsReauthorization {
                    <Button
                        size={Size::Small}
                        on_click={onreconnect}
                        loading={*reconnecting}
                        disabled={*removing}
                    >
                        { "Reconnect" }
                    </Button>
                } else if connection.kind == ConnectionKind::ApiKey {
                    <Button size={Size::Small} on_click={onedit} disabled={*removing}>
                        { "Edit" }
                    </Button>
                }
                <Button
                    variant={ButtonVariant::Danger}
                    size={Size::Small}
                    on_click={onask_remove}
                    loading={*removing}
                    disabled={*reconnecting}
                >
                    { "Unlink" }
                </Button>
            </ButtonGroup>
            if let Some(message) = &*reconnect_error {
                <Text
                    tag={TextTag::P}
                    size={TextSize::Sm}
                    tone={Tone::Danger}
                    class="connection__error"
                    attrs={attrs! { "role" => "alert" }}
                >
                    { message }
                </Text>
            }
            if *editing {
                <EditConnection connection={connection.clone()} {onsaved} oncancel={oncancel_edit} />
            }
            <ConfirmDialog
                open={*confirming_remove}
                title={format!("Unlink {}?", connection.name)}
                confirm_label="Unlink"
                danger=true
                busy={*removing}
                on_confirm={onremove}
                on_cancel={oncancel_remove}
            >
                { "Workflows that publish through it will stop working." }
            </ConfirmDialog>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct EditConnectionProps {
    connection: ConnectionSummary,
    onsaved: Callback<()>,
    oncancel: Callback<()>,
}

#[function_component(EditConnection)]
fn edit_connection(props: &EditConnectionProps) -> Html {
    let name = use_state(|| props.connection.name.clone());
    let key = use_state(String::new);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    let on_name = {
        let name = name.clone();
        Callback::from(move |value: String| name.set(value))
    };
    let on_key = {
        let key = key.clone();
        Callback::from(move |value: String| key.set(value))
    };
    let on_cancel = {
        let oncancel = props.oncancel.clone();
        Callback::from(move |_: MouseEvent| oncancel.emit(()))
    };
    let onsubmit = {
        let id = props.connection.id.to_string();
        let name = name.clone();
        let key = key.clone();
        let busy = busy.clone();
        let error = error.clone();
        let onsaved = props.onsaved.clone();

        Callback::from(move |_: MouseEvent| {
            let id = id.clone();
            let name = name.trim().to_string();
            let key = key.trim().to_string();
            let busy = busy.clone();
            let error = error.clone();
            let onsaved = onsaved.clone();
            busy.set(true);
            error.set(None);

            wasm_bindgen_futures::spawn_local(async move {
                match api::update_service_connection(
                    &id,
                    &name,
                    (!key.is_empty()).then_some(key.as_str()),
                )
                .await
                {
                    Ok(_) => onsaved.emit(()),
                    Err(err) => error.set(Some(err.to_string())),
                }
                busy.set(false);
            });
        })
    };

    let footer = html! {
        <>
            <Button variant={ButtonVariant::Subtle} on_click={on_cancel} disabled={*busy}>
                { "Cancel" }
            </Button>
            <Button
                variant={ButtonVariant::Primary}
                on_click={onsubmit}
                loading={*busy}
                disabled={name.trim().is_empty()}
            >
                { "Save changes" }
            </Button>
        </>
    };

    html! {
        <Dialog
            open=true
            title={format!("Edit {} connection", props.connection.name)}
            {footer}
            on_close={props.oncancel.clone()}
        >
            <Stack>
                if let Some(message) = &*error {
                    <Alert tone={Tone::Danger} title="We could not update that connection.">
                        { message.clone() }
                    </Alert>
                }
                <Field label="Name" required=true>
                    <TextInput value={(*name).clone()} on_input={on_name} />
                </Field>
                <Field
                    label="New API key"
                    hint="Write-only. Leave blank to keep the existing API key."
                >
                    <SecretInput
                        value={(*key).clone()}
                        on_input={on_key}
                        placeholder="Paste a replacement API key"
                    />
                </Field>
            </Stack>
        </Dialog>
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
    let (tone, label) = match props.status {
        ConnectionStatus::Ok => return html! {},
        ConnectionStatus::NeedsReauthorization => (Tone::Warn, "Needs reconnecting"),
        ConnectionStatus::Error => (Tone::Danger, "Not working"),
    };

    html! { <Pill {tone} dot=true class="status-pill">{ label }</Pill> }
}

fn kind_label(kind: ConnectionKind) -> &'static str {
    match kind {
        ConnectionKind::OAuth2 => "authorised",
        ConnectionKind::ApiKey => "token",
        ConnectionKind::GitHubApp => "app installation",
    }
}

fn account_type(connection: &ConnectionSummary) -> Option<&str> {
    connection
        .metadata
        .get("account_type")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
}
