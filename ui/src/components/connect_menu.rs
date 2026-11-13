//! A connection menu which combines locally handled credential choices with
//! integrations configured on the agent. Selecting an integration starts a
//! bearer-authenticated request that mints a provider authorization URL, which
//! is opened in a popup (the provider redirects back to the agent's
//! server-rendered callback, which records the resulting connection). The main
//! page is never navigated away from.

use automate_api::IntegrationInfo;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yewi::prelude::*;

use crate::api;

fn default_label() -> AttrValue {
    "Connect".into()
}

#[derive(Properties, PartialEq)]
pub struct ConnectMenuProps {
    /// Fired once a setup flow has been started and its popup opened. The
    /// connection itself is completed in that popup, so a caller that wants to
    /// show the result should refresh on a later interaction as well.
    #[prop_or_default]
    pub onstarted: Callback<()>,

    /// Services that are linked by pasting a credential into Automate, as
    /// `(id, label)` pairs.
    #[prop_or_default]
    pub credential_options: Vec<(AttrValue, AttrValue)>,

    /// Fired with the id of a pasted-credential service to start adding it.
    #[prop_or_default]
    pub oncredential: Callback<String>,

    /// The button's visible label.
    #[prop_or_else(default_label)]
    pub label: AttrValue,

    #[prop_or_default]
    pub variant: ButtonVariant,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or(Size::Small)]
    pub size: Size,
}

/// The menu of services an account can be connected to.
///
/// Services linked by pasting a credential come from the caller; services that
/// are authorised in a popup come from the agent's list of integrations. When
/// both exist they are grouped under headings so the two ways of connecting are
/// not mistaken for one another.
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
                    Err(err) => error.set(Some(err.to_string())),
                }
            });
            || ()
        });
    }

    let has_credentials = !props.credential_options.is_empty();

    // With nothing to offer there is no menu to open. If that is because the
    // integrations could not be fetched, say so instead of vanishing.
    if !has_credentials && integrations.is_empty() {
        return match (*error).clone() {
            Some(message) => html! {
                <span class="connect-menu__error" role="status" title={message}>
                    { "Connect unavailable" }
                </span>
            },
            None => html! {},
        };
    }

    let credentials = props.credential_options.iter().map(|(id, label)| {
        let id = id.to_string();
        let on_select = props.oncredential.reform(move |_: MouseEvent| id.clone());
        html! { <MenuItem {on_select}>{ label.clone() }</MenuItem> }
    });

    let setups = integrations.iter().map(|integration| {
        let id = integration.id.clone();
        let error = error.clone();
        let onstarted = props.onstarted.clone();
        let on_select = Callback::from(move |_: MouseEvent| {
            let (id, error, onstarted) = (id.clone(), error.clone(), onstarted.clone());
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
        html! { <MenuItem {on_select}>{ integration.name.clone() }</MenuItem> }
    });

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
                icon={icons::LINK}
                variant={props.variant}
                size={props.size}
                disabled={props.disabled}
            >
                if has_credentials {
                    <MenuGroup label="API keys">{ for credentials }</MenuGroup>
                    if !integrations.is_empty() {
                        <MenuGroup label="Authorized accounts">{ for setups }</MenuGroup>
                    }
                } else {
                    { for setups }
                }
            </MenuButton>
            { error_status }
        </>
    }
}
