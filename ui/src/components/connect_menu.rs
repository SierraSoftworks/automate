//! A connection menu which combines locally handled credential choices with
//! integrations configured on the agent. Selecting an integration starts a
//! bearer-authenticated request that mints a provider authorization URL, which
//! is opened in a popup (the provider redirects back to the agent's
//! server-rendered callback, which records the resulting connection). The main
//! page is never navigated away from.

use automate_api::IntegrationInfo;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use super::{form::ButtonKind, menu_button::MenuButton, menu_button::MenuButtonOption};
use crate::api;

const CREDENTIAL_PREFIX: &str = "credential:";
const SETUP_PREFIX: &str = "setup:";

fn default_label() -> AttrValue {
    "Connect".into()
}

fn default_title() -> Option<AttrValue> {
    Some("Connect an integration".into())
}

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
#[derive(Properties, PartialEq)]
pub struct ConnectMenuProps {
    #[prop_or_default]
    pub onstarted: Callback<()>,

    #[prop_or_default]
    pub credential_options: Vec<MenuButtonOption>,

    #[prop_or_default]
    pub oncredential: Callback<String>,

    #[prop_or_else(default_label)]
    pub label: AttrValue,

    #[prop_or_default]
    pub kind: ButtonKind,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or(true)]
    pub small: bool,

    #[prop_or_else(default_title)]
    pub title: Option<AttrValue>,
}

/// A toolbar control that reveals credential choices supplied by its parent and
/// guided setup integrations fetched from the agent.
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

    let has_credentials = !props.credential_options.is_empty();
    let mut options: Vec<MenuButtonOption> = props
        .credential_options
        .iter()
        .map(|option| {
            MenuButtonOption::new(
                format!("{CREDENTIAL_PREFIX}{}", option.value),
                option.label.clone(),
            )
            .in_section("API keys")
        })
        .collect();
    options.extend(integrations.iter().map(|integration| {
        let option = MenuButtonOption::new(
            format!("{SETUP_PREFIX}{}", integration.id),
            integration.name.clone(),
        );
        if has_credentials {
            option.in_section("Authorized accounts")
        } else {
            option
        }
    }));

    if options.is_empty() {
        return match (*error).clone() {
            Some(message) => html! {
                <span class="connect-menu__error" role="status" title={message}>
                    { "Connect unavailable" }
                </span>
            },
            None => html! {},
        };
    }

    let onselect = {
        let error = error.clone();
        let onstarted = props.onstarted.clone();
        let oncredential = props.oncredential.clone();
        Callback::from(move |value: String| {
            if let Some(value) = value.strip_prefix(CREDENTIAL_PREFIX) {
                oncredential.emit(value.to_string());
                return;
            }

            let Some(id) = value.strip_prefix(SETUP_PREFIX).map(str::to_string) else {
                return;
            };
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

    let error_status = (*error).clone().map(|message| {
        html! {
            <span class="connect-menu__error" role="status" title={message}>
                { "Some connection methods unavailable" }
            </span>
        }
    });

    html! {
        <>
            <MenuButton
                label={props.label.clone()}
                {options}
                {onselect}
                kind={props.kind}
                disabled={props.disabled}
                small={props.small}
                title={props.title.clone()}
            >
                <span class="menu-button__icon" aria-hidden="true">{ connect_icon() }</span>
            </MenuButton>
            { error_status }
        </>
    }
}
