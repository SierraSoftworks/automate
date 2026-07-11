use yew::prelude::*;
use yew_router::prelude::*;

use crate::app::{AuthHandle, AuthStatus, Route};
use crate::components::Layout;
use crate::fixtures;
use crate::util;

/// The public landing page shown at the site root. It introduces Automate and
/// funnels visitors into the admin area with a single sign-in click.
#[function_component(Landing)]
pub fn landing() -> Html {
    let auth = use_context::<AuthHandle>();
    let navigator = use_navigator();

    // Once the session resolves to anything other than "needs login", send the
    // visitor straight to the admin area: an already signed-in visitor never has
    // to click through the landing page, and a fresh sign-in lands them in the
    // admin area directly. The shared auth context already carries the resolved
    // (and access-granting) state, so this client-side navigation needs no
    // full-page reload — the admin gate sees the signed-in status immediately. A
    // Forbidden/Errored session is sent through too, so the admin gate can
    // explain why. Demo mode navigates full-page so the `?demo` flag is kept.
    {
        let navigator = navigator.clone();
        let status = auth.as_ref().map(|handle| handle.status.clone());
        use_effect_with(status, move |status| {
            let resolved = !matches!(
                status,
                None | Some(AuthStatus::Loading) | Some(AuthStatus::NeedsLogin)
            );
            if resolved {
                if fixtures::is_demo() {
                    if let Some(window) = web_sys::window() {
                        let _ = window.location().set_href(&util::nav_href("/admin"));
                    }
                } else if let Some(nav) = navigator.clone() {
                    nav.push(&Route::AdminRoot);
                }
            }
            || ()
        });
    }

    let action = match auth.as_ref() {
        // Authentication is required: offer the single sign-in action. On success
        // the effect above redirects into the admin area.
        Some(handle) if handle.status == AuthStatus::NeedsLogin => {
            let login = handle.login.clone();
            let onclick = Callback::from(move |_: MouseEvent| login.emit(()));
            html! {
                <button class="btn btn--primary btn--lg" {onclick}>
                    { "Sign in" }
                </button>
            }
        }
        // Either still resolving the session, or already resolved and about to
        // redirect to the admin area: show a disabled placeholder so the primary
        // call to action doesn't flicker between states.
        _ => html! {
            <button class="btn btn--primary btn--lg" disabled={true}>
                { "Sign in" }
            </button>
        },
    };

    html! {
        <Layout>
            <main class="landing">
                <div class="landing__inner">
                    <h1 class="landing__title">{ "Automate" }</h1>
                    <p class="landing__lead">
                        { "A simple, self-hosted automation platform. Automate syncs \
                           calendars, syndicates feeds, manages GitHub notifications, \
                           and routes webhooks — using Todoist to ask for a human when \
                           it needs one." }
                    </p>
                    <div class="landing__actions">
                        { action }
                    </div>
                </div>
            </main>
        </Layout>
    }
}
