use yew::prelude::*;
use yew_router::prelude::use_navigator;

use crate::app::{AuthHandle, AuthStatus, Route};
use yewi::prelude::*;

use crate::components::{Footer, brand};
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

    let login = auth
        .as_ref()
        .filter(|handle| handle.status == AuthStatus::NeedsLogin)
        .map(|handle| handle.login.reform(|_: MouseEvent| ()));

    html! {
        <AppShell bar={html! { <AppBar brand={brand()} actions={html! { <ThemeToggle /> }} /> }} class="app-shell app-shell--bare">
            <div class="landing">
                <Heading level={HeadingLevel::H1} size={TextSize::Xxxl}>{ "Automate" }</Heading>
                <Text tag={TextTag::P} size={TextSize::Lg} color={TextColor::Muted}>
                    { "A simple, self-hosted automation platform. Automate syncs \
                       calendars, syndicates feeds, manages GitHub notifications, \
                       and routes webhooks — using Todoist to ask for a human when \
                       it needs one." }
                </Text>
                <Button
                    variant={ButtonVariant::Primary}
                    size={Size::Large}
                    icon={icons::LOG_IN}
                    disabled={login.is_none()}
                    on_click={login.unwrap_or_default()}
                >
                    { "Sign in" }
                </Button>
            </div>
            <Footer />
        </AppShell>
    }
}
