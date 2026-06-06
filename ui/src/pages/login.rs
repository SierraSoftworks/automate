use yew::prelude::*;

use crate::app::AuthHandle;
use crate::components::Center;

/// The sign-in prompt shown when authentication is required but no valid
/// session exists.
#[function_component(Login)]
pub fn login() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");

    let onclick = {
        let login = auth.login.clone();
        Callback::from(move |_: MouseEvent| login.emit(()))
    };

    html! {
        <Center>
            <div class="auth-card">
                <h1>{ "Sign in" }</h1>
                <p>{ "You need to sign in to access the Automate admin area." }</p>
                <button {onclick}>{ "Sign in" }</button>
            </div>
        </Center>
    }
}
