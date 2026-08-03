//! The connections panel: for each integration configured on the agent, the
//! accounts currently connected to it, with a control to sever each one.
//!
//! Disconnecting is destructive and not undoable from here — for GitHub it
//! uninstalls the App from the account — so it is confirmed before the request
//! is sent.

use automate_api::{Connection, IntegrationInfo};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{Alert, AlertKind};
use crate::fixtures;

/// One integration and whatever we currently know about its connections.
#[derive(Clone, PartialEq)]
struct Row {
    integration: IntegrationInfo,
    connections: Vec<Connection>,
    error: Option<String>,
}

#[derive(Properties, PartialEq, Default)]
pub struct ConnectionsPanelProps {
    /// Bumped by the surrounding page to force a reload — for example once a
    /// setup popup has been opened.
    #[prop_or_default]
    pub reload: u32,
}

async fn load(rows: UseStateHandle<Option<Vec<Row>>>, error: UseStateHandle<Option<String>>) {
    let integrations = match api::list_integrations().await {
        Ok(integrations) => integrations,
        Err(err) => {
            error.set(Some(err.to_string()));
            return;
        }
    };

    let mut loaded = Vec::with_capacity(integrations.len());
    for integration in integrations {
        // One integration failing must not hide the rest, so the failure is
        // recorded against its own row.
        let (connections, error) = match api::list_connections(&integration.id).await {
            Ok(connections) => (connections, None),
            Err(err) => (vec![], Some(err.to_string())),
        };

        loaded.push(Row {
            integration,
            connections,
            error,
        });
    }

    error.set(None);
    rows.set(Some(loaded));
}

#[function_component(ConnectionsPanel)]
pub fn connections_panel(props: &ConnectionsPanelProps) -> Html {
    let rows = use_state(|| Option::<Vec<Row>>::None);
    let error = use_state(|| Option::<String>::None);

    {
        let rows = rows.clone();
        let error = error.clone();
        use_effect_with(props.reload, move |_| {
            if !fixtures::is_demo() {
                spawn_local(load(rows, error));
            }
            || ()
        });
    }

    let reload = {
        let rows = rows.clone();
        let error = error.clone();
        Callback::from(move |_: ()| spawn_local(load(rows.clone(), error.clone())))
    };

    if let Some(message) = (*error).clone() {
        return html! {
            <section class="connections">
                <h2 class="connections__heading">{ "Connections" }</h2>
                <Alert kind={AlertKind::Error} title="Couldn't load the connections" message={message} />
            </section>
        };
    }

    let Some(rows) = (*rows).clone() else {
        return html! {};
    };

    if rows.is_empty() {
        return html! {};
    }

    let sections = rows
        .iter()
        .map(|row| {
            let body = if let Some(message) = &row.error {
                html! {
                    <Alert
                        kind={AlertKind::Error}
                        title="Couldn't load these connections"
                        message={message.clone()}
                    />
                }
            } else if row.connections.is_empty() {
                html! { <p class="connections__empty">{ "Not connected." }</p> }
            } else {
                let items = row
                    .connections
                    .iter()
                    .map(|connection| {
                        let integration_id = row.integration.id.clone();
                        let connection_id = connection.id.clone();
                        let label = connection.name.clone();
                        let reload = reload.clone();

                        let ondisconnect = Callback::from(move |_: MouseEvent| {
                            let confirmed = web_sys::window()
                                .and_then(|w| {
                                    w.confirm_with_message(&format!(
                                        "Disconnect {label}? This cannot be undone from Automate."
                                    ))
                                    .ok()
                                })
                                .unwrap_or(false);

                            if !confirmed {
                                return;
                            }

                            let integration_id = integration_id.clone();
                            let connection_id = connection_id.clone();
                            let reload = reload.clone();
                            spawn_local(async move {
                                let _ = api::disconnect(&integration_id, &connection_id).await;
                                reload.emit(());
                            });
                        });

                        html! {
                            <li class="connections__item">
                                <span class="connections__name">{ &connection.name }</span>
                                if let Some(kind) = &connection.kind {
                                    <span class="connections__kind">{ kind }</span>
                                }
                                if let Some(detail) = &connection.detail {
                                    <span class="connections__detail">{ detail }</span>
                                }
                                <button
                                    class="btn btn--small btn--danger"
                                    onclick={ondisconnect}
                                    title="Disconnect this account"
                                >
                                    { "Disconnect" }
                                </button>
                            </li>
                        }
                    })
                    .collect::<Html>();

                html! { <ul class="connections__list">{ items }</ul> }
            };

            html! {
                <div class="connections__group">
                    <h3 class="connections__title">{ &row.integration.name }</h3>
                    { body }
                </div>
            }
        })
        .collect::<Html>();

    html! {
        <section class="connections">
            <h2 class="connections__heading">{ "Connections" }</h2>
            { sections }
        </section>
    }
}
