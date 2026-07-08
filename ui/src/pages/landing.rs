use yew::prelude::*;

use crate::app::{AuthHandle, AuthStatus};
use crate::components::Layout;
use crate::util;

/// The public landing page shown at the site root. It introduces Automate and
/// links through to the admin area.
#[function_component(Landing)]
pub fn landing() -> Html {
    let auth = use_context::<AuthHandle>();

    // When authentication is required, open the sign-in popup straight away
    // (the visitor can then click through to the admin area once signed in)
    // instead of routing them to an intermediate sign-in screen. In every other
    // state a plain link to the admin area is enough.
    let action = match auth.as_ref() {
        Some(handle) if handle.status == AuthStatus::NeedsLogin => {
            let login = handle.login.clone();
            let onclick = Callback::from(move |_: MouseEvent| login.emit(()));
            html! {
                <button class="btn btn--primary btn--lg" {onclick}>
                    { "Sign in to open the admin area" }
                </button>
            }
        }
        _ => html! {
            <a class="btn btn--primary btn--lg" href={util::nav_href("/admin")}>
                { "Open the admin area" }
            </a>
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
