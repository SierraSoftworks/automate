use yew::prelude::*;
use yewi::prelude::*;

use crate::app::AuthHandle;

/// The sign-in prompt shown when authentication is required but no valid
/// session exists. Lives within the admin shell at `/admin/`.
#[function_component(Login)]
pub fn login() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");

    let onclick = {
        let login = auth.login.clone();
        Callback::from(move |_: MouseEvent| login.emit(()))
    };

    html! {
        <EmptyState
            class="auth-screen"
            title="Sign in"
            icon={icons::LOCK_KEYHOLE}
            actions={html! {
                <Button variant={ButtonVariant::Primary} icon={icons::LOG_IN} on_click={onclick}>
                    { "Sign in" }
                </Button>
            }}
        >
            { "You need to sign in to access the Automate admin area." }
        </EmptyState>
    }
}
