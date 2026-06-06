use yew::prelude::*;

use crate::app::{AuthHandle, AuthStatus};
use crate::components::Center;

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
            <Center><p>{ "Loading…" }</p></Center>
        },
        AuthStatus::NeedsLogin => html! { <Login /> },
        AuthStatus::Error(msg) => html! {
            <Center>
                <div class="auth-card">
                    <h1>{ "Something went wrong" }</h1>
                    <p class="auth-error">{ msg }</p>
                </div>
            </Center>
        },
        AuthStatus::SignedIn(_) | AuthStatus::Disabled => props.children.clone(),
    }
}
