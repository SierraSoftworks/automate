use yew::prelude::*;
use yew_router::prelude::*;

use crate::app::{AuthHandle, AuthStatus, Route};

/// The OIDC login callback page (`/auth/callback`). The provider redirects the
/// login popup here with `?code&state`; the [`use_auth`](crate::app) hook performs
/// the actual code exchange on mount. In a popup the window hands its tokens back
/// to the opener and closes itself before this matters; on a direct-navigation
/// fallback the session is established and this view sends the user straight into
/// the admin area.
#[function_component(AuthCallback)]
pub fn auth_callback() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let navigator = use_navigator();

    {
        let navigator = navigator.clone();
        // Once the callback has resolved (anything but the initial Loading state),
        // continue into the admin area. In a popup the window closes itself first,
        // so this only runs on the direct-navigation fallback.
        use_effect_with(auth.status.clone(), move |status| {
            if !matches!(status, AuthStatus::Loading)
                && let Some(nav) = navigator.clone()
            {
                nav.push(&Route::AdminRoot);
            }
            || ()
        });
    }

    html! {
        <div class="auth-screen">
            <div class="auth-card">
                <p class="auth-card__lead">{ "Completing sign-in…" }</p>
            </div>
        </div>
    }
}
