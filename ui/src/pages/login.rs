use yew::prelude::*;

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
        <div class="auth-screen">
            <div class="auth-card">
                <h1 class="auth-card__title">{ "Sign in" }</h1>
                <p class="auth-card__lead">
                    { "You need to sign in to access the Automate admin area." }
                </p>
                <button class="btn btn--primary btn--lg" {onclick}>{ "Sign in" }</button>
            </div>
        </div>
    }
}
