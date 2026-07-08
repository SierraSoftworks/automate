//! The integrations panel: lists the agent's configured OAuth providers and lets
//! an administrator connect each one. Connecting starts a bearer-authenticated
//! request that mints a provider authorization URL, which is then opened in a
//! popup (the provider redirects back to the agent's server-rendered callback,
//! which stores the resulting token). The main page is never navigated away from.

use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::{self, OAuthProvider};
use crate::fixtures;

#[function_component(Integrations)]
pub fn integrations() -> Html {
    let providers = use_state(Vec::<OAuthProvider>::new);

    {
        let providers = providers.clone();
        use_effect_with((), move |_| {
            // The listing is admin-gated; in demo mode there is no agent to ask.
            if !fixtures::is_demo() {
                spawn_local(async move {
                    if let Ok(list) = api::list_oauth_providers().await {
                        providers.set(list);
                    }
                });
            }
            || ()
        });
    }

    // Nothing configured (or not permitted): render nothing rather than an empty
    // panel.
    if providers.is_empty() {
        return html! {};
    }

    let items = providers
        .iter()
        .map(|provider| {
            let key = provider.provider.clone();
            let onclick = Callback::from(move |_: MouseEvent| {
                let key = key.clone();
                spawn_local(async move {
                    if let Ok(url) = api::start_oauth(&key).await
                        && let Some(window) = web_sys::window()
                    {
                        let _ = window.open_with_url_and_target_and_features(
                            &url,
                            "automate-connect",
                            "popup,width=480,height=720",
                        );
                    }
                });
            });

            html! {
                <li class="integration">
                    <span class="integration__name">{ &provider.name }</span>
                    <button class="btn btn--small btn--primary" {onclick}>{ "Connect" }</button>
                </li>
            }
        })
        .collect::<Html>();

    html! {
        <section class="integrations">
            <h2 class="integrations__title">{ "Integrations" }</h2>
            <ul class="integrations__list">{ items }</ul>
        </section>
    }
}
