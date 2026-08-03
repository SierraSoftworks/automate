//! The Connect menu: a themed dropdown shown beside the page's Refresh control
//! that lists the integrations configured on the agent. Selecting one starts a
//! bearer-authenticated request that mints a provider authorization URL, which
//! is opened in a popup (the provider redirects back to the agent's
//! server-rendered callback, which records the resulting connection). The main
//! page is never navigated away from.

use automate_api::IntegrationInfo;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::fixtures;

/// A link/plug glyph shown on the trigger, echoing the "connect" action.
fn connect_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" />
            <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" />
        </svg>
    }
}

/// A downward chevron shown on the trigger; it rotates when the menu is open.
fn chevron_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polyline points="6 9 12 15 18 9" />
        </svg>
    }
}

/// Emitted when a setup popup has been opened, so the surrounding page can
/// refresh whatever it shows about connections once the user comes back.
#[derive(Properties, PartialEq, Default)]
pub struct ConnectMenuProps {
    #[prop_or_default]
    pub onstarted: Callback<()>,
}

/// A compact toolbar control that reveals a themed dropdown of the configured
/// integrations. Renders nothing when none are configured (for example in demo
/// mode), so it never leaves an empty button in the toolbar.
#[function_component(ConnectMenu)]
pub fn connect_menu(props: &ConnectMenuProps) -> Html {
    let integrations = use_state(Vec::<IntegrationInfo>::new);
    let error = use_state(|| Option::<String>::None);
    let open = use_state(|| false);

    {
        let integrations = integrations.clone();
        let error = error.clone();
        use_effect_with((), move |_| {
            // The listing is admin-gated; in demo mode there is no agent to ask.
            if !fixtures::is_demo() {
                spawn_local(async move {
                    match api::list_integrations().await {
                        Ok(list) => integrations.set(list),
                        // Worth saying out loud: an empty menu and a broken
                        // endpoint look identical otherwise, which is how a
                        // contract change can go unnoticed.
                        Err(err) => error.set(Some(err.to_string())),
                    }
                });
            }
            || ()
        });
    }

    if let Some(message) = (*error).clone() {
        return html! {
            <span class="connect-menu__error" role="status" title={message.clone()}>
                { "Connect unavailable" }
            </span>
        };
    }

    // Nothing configured: render nothing rather than an empty control in the
    // toolbar.
    if integrations.is_empty() {
        return html! {};
    }

    let toggle = {
        let open = open.clone();
        Callback::from(move |_: MouseEvent| open.set(!*open))
    };

    // Escape closes the menu from anywhere within it.
    let onkeydown = {
        let open = open.clone();
        Callback::from(move |e: KeyboardEvent| {
            if e.key() == "Escape" {
                open.set(false);
            }
        })
    };

    let menu = if *open {
        // A transparent, full-viewport backdrop behind the list closes the menu
        // when the user clicks anywhere outside it (the list sits above it).
        let close = {
            let open = open.clone();
            Callback::from(move |_: MouseEvent| open.set(false))
        };

        let items = integrations
            .iter()
            .map(|integration| {
                let id = integration.id.clone();
                let open = open.clone();
                let error = error.clone();
                let onstarted = props.onstarted.clone();
                let onclick = Callback::from(move |_: MouseEvent| {
                    let id = id.clone();
                    let error = error.clone();
                    let onstarted = onstarted.clone();
                    open.set(false);
                    spawn_local(async move {
                        match api::start_setup(&id).await {
                            Ok(url) => {
                                if let Some(window) = web_sys::window() {
                                    let _ = window.open_with_url_and_target_and_features(
                                        &url,
                                        "automate-connect",
                                        "popup,width=480,height=720",
                                    );
                                }
                                onstarted.emit(());
                            }
                            Err(err) => error.set(Some(err.to_string())),
                        }
                    });
                });

                html! {
                    <li role="none">
                        <button class="connect-menu__item" role="menuitem" {onclick}>
                            { &integration.name }
                        </button>
                    </li>
                }
            })
            .collect::<Html>();

        html! {
            <>
                <div class="connect-menu__backdrop" onclick={close} />
                <ul class="connect-menu__list" role="menu">{ items }</ul>
            </>
        }
    } else {
        html! {}
    };

    let mut chevron_class = classes!("connect-menu__chevron");
    if *open {
        chevron_class.push("connect-menu__chevron--open");
    }

    html! {
        <div class="connect-menu" {onkeydown}>
            <button
                class="btn btn--small"
                onclick={toggle}
                aria-haspopup="menu"
                aria-expanded={(*open).to_string()}
                title="Connect an integration"
            >
                <span class="connect-menu__icon" aria-hidden="true">{ connect_icon() }</span>
                { "Connect" }
                <span class={chevron_class} aria-hidden="true">{ chevron_icon() }</span>
            </button>
            { menu }
        </div>
    }
}
