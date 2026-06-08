use yew::prelude::*;

use crate::components::Layout;
use crate::util;

/// The public landing page shown at the site root. It introduces Automate and
/// links through to the admin area.
#[function_component(Landing)]
pub fn landing() -> Html {
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
                        <a class="btn btn--primary btn--lg" href={util::nav_href("/admin")}>
                            { "Open the admin area" }
                        </a>
                    </div>
                </div>
            </main>
        </Layout>
    }
}
