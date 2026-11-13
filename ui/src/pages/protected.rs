use yew::prelude::*;

use crate::app::{AuthHandle, AuthStatus};
use yewi::prelude::*;

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
        AuthStatus::Loading => html! { <Loading label="Loading…" layout={LoadingLayout::Block} /> },
        AuthStatus::NeedsLogin => html! { <Login /> },
        AuthStatus::Forbidden => html! {
            <Alert tone={Tone::Danger} title="Access denied">
                { "Your request was not permitted by the admin access-control policy. \
                   If this is unexpected, check the agent's `[web.auth]` ACL configuration." }
            </Alert>
        },
        AuthStatus::Error(msg) => {
            let on_retry = Callback::from(|_| {
                if let Some(window) = web_sys::window() {
                    let _ = window.location().reload();
                }
            });
            html! {
                <ErrorState title="Couldn't verify your session" {on_retry} retry_label="Reload">
                    { msg }
                </ErrorState>
            }
        }
        AuthStatus::SignedIn(_) | AuthStatus::Disabled => props.children.clone(),
    }
}
