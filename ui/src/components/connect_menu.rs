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
use super::menu_button::{MenuButton, MenuButtonOption};

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

/// Emitted when a setup popup has been opened, so the surrounding page can
/// refresh whatever it shows about connections once the user comes back.
#[derive(Properties, PartialEq, Default)]
pub struct ConnectMenuProps {
    #[prop_or_default]
    pub onstarted: Callback<()>,
}

/// A compact toolbar control that reveals a themed dropdown of the configured
/// integrations. Renders nothing when none are configured, so it never leaves an
/// empty button in the toolbar.
#[function_component(ConnectMenu)]
pub fn connect_menu(props: &ConnectMenuProps) -> Html {
    let integrations = use_state(Vec::<IntegrationInfo>::new);
    let error = use_state(|| Option::<String>::None);

    {
        let integrations = integrations.clone();
        let error = error.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                match api::list_integrations().await {
                    Ok(list) => integrations.set(list),
                    // Worth saying out loud: an empty menu and a broken
                    // endpoint look identical otherwise, which is how a
                    // contract change can go unnoticed.
                    Err(err) => error.set(Some(err.to_string())),
                }
            });
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

    let options: Vec<MenuButtonOption> = integrations
        .iter()
        .map(|integration| MenuButtonOption::new(integration.id.clone(), integration.name.clone()))
        .collect();

    let onselect = {
        let error = error.clone();
        let onstarted = props.onstarted.clone();
        Callback::from(move |id: String| {
            let error = error.clone();
            let onstarted = onstarted.clone();
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
        })
    };

    html! {
        <MenuButton
            label="Connect"
            {options}
            {onselect}
            small=true
            title="Connect an integration"
        >
            <span class="menu-button__icon" aria-hidden="true">{ connect_icon() }</span>
        </MenuButton>
    }
}
