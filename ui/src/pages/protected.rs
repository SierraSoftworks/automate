use yew::prelude::*;

use crate::app::{AuthHandle, AuthStatus};
use crate::components::{Alert, AlertKind};

use super::Login;

#[derive(Properties, PartialEq)]
pub struct ProtectedProps {
    #[prop_or_default]
    pub children: Html,
}

/// Gates its children behind the resolved authentication state. The children are
/// only mounted (and therefore only fetch data) once access is granted.
#[function_component(Protected)]
pub fn protected(props: &ProtectedProps) -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");

    match auth.status {
        AuthStatus::Loading => html! {
            <p class="loading-note">{ "Loading…" }</p>
        },
        AuthStatus::NeedsLogin => html! { <Login /> },
        AuthStatus::Forbidden => html! {
            <Alert
                kind={AlertKind::Error}
                title="Access denied"
                message="Your request was not permitted by the admin access-control policy. \
                    If this is unexpected, check the agent's `[web.admin]` acl configuration."
            />
        },
        AuthStatus::Error(msg) => {
            let onclick = Callback::from(|_: MouseEvent| {
                if let Some(window) = web_sys::window() {
                    let _ = window.location().reload();
                }
            });
            html! {
                <Alert
                    kind={AlertKind::Error}
                    title="Couldn't verify your session"
                    message={msg}
                >
                    <button class="btn btn--small btn--primary" {onclick}>{ "Reload" }</button>
                </Alert>
            }
        }
        AuthStatus::SignedIn(_) | AuthStatus::Disabled => props.children.clone(),
    }
}
