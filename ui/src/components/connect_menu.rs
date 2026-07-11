//! The Connect menu: a themed dropdown shown beside the page's Refresh control
//! that lists the agent's configured OAuth integration providers. Selecting a
//! provider starts a bearer-authenticated request that mints a provider
//! authorization URL, which is opened in a popup (the provider redirects back to
//! the agent's server-rendered callback, which stores the resulting token). The
//! main page is never navigated away from.

use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::{self, OAuthProvider};
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

/// A compact toolbar control that reveals a themed dropdown of the configured
/// integration providers. Renders nothing when no providers are available (for
/// example in demo mode, or when the caller lacks permission), so it never
/// leaves an empty button in the toolbar.
#[function_component(ConnectMenu)]
pub fn connect_menu() -> Html {
    let providers = use_state(Vec::<OAuthProvider>::new);
    let open = use_state(|| false);

    {
        let providers = providers.clone();
        use_effect_with((), move |_| {
            // The listing is admin-gated; in demo mode there is no agent to ask.
            if !fixtures::is_demo() {
                spawn_local(async move {
                    if let Ok(list) = api::list_oauth_providers().await {
                        providers.set(list);
                    }
                });
            }
            || ()
        });
    }

    // Nothing configured (or not permitted): render nothing rather than an empty
    // control in the toolbar.
    if providers.is_empty() {
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

        let items = providers
            .iter()
            .map(|provider| {
                let key = provider.provider.clone();
                let open = open.clone();
                let onclick = Callback::from(move |_: MouseEvent| {
                    let key = key.clone();
                    open.set(false);
                    spawn_local(async move {
                        if let Ok(url) = api::start_oauth(&key).await
                            && let Some(window) = web_sys::window()
                        {
                            let _ = window.open_with_url_and_target_and_features(
                                &url,
                                "automate-connect",
                                "popup,width=480,height=720",
                            );
                        }
                    });
                });

                html! {
                    <li role="none">
                        <button class="connect-menu__item" role="menuitem" {onclick}>
                            { &provider.name }
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
