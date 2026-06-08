use yew::prelude::*;

use crate::Route;
use crate::app::{AuthHandle, AuthStatus};
use crate::util;

#[derive(Properties, PartialEq)]
pub struct AppBarProps {
    /// The currently active admin route, used to highlight the matching nav link.
    pub active: Route,
}

/// A single primary navigation entry in the app bar.
struct NavItem {
    label: &'static str,
    path: &'static str,
    route: Route,
}

/// Derives up to two uppercase initials from a display name or email address.
fn initials(name: &str) -> String {
    let from_words: String = name
        .split(|c: char| c.is_whitespace() || c == '.' || c == '@' || c == '_' || c == '-')
        .filter(|w| !w.is_empty())
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect();

    let initials = if from_words.is_empty() {
        name.chars().take(2).collect()
    } else {
        from_words
    };

    initials.to_uppercase()
}

/// The persistent top-level application bar shown across every admin view. It
/// hosts the brand mark, primary navigation, and the signed-in user chip, and
/// stays consistent as the user moves between pages.
#[function_component(AppBar)]
pub fn app_bar(props: &AppBarProps) -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");

    let signed_in = matches!(auth.status, AuthStatus::SignedIn(_) | AuthStatus::Disabled);

    let nav_items = [
        NavItem {
            label: "Dashboard",
            path: "/admin",
            route: Route::Dashboard,
        },
        NavItem {
            label: "Key-Value",
            path: "/admin/db",
            route: Route::Db,
        },
        NavItem {
            label: "Queue",
            path: "/admin/queue",
            route: Route::Queue,
        },
    ];

    let nav = if signed_in {
        html! {
            <nav class="app-bar__nav">
                { for nav_items.iter().map(|item| {
                    let mut class = classes!("app-bar__link");
                    if item.route == props.active {
                        class.push("app-bar__link--active");
                    }
                    html! {
                        <a class={class} href={util::nav_href(item.path)}>{ item.label }</a>
                    }
                }) }
            </nav>
        }
    } else {
        html! {}
    };

    let user = match &auth.user {
        Some(user) => {
            let on_signout = {
                let signout = auth.signout.clone();
                Callback::from(move |_: MouseEvent| signout.emit(()))
            };
            let email = match &user.email {
                Some(email) => html! { <span class="user-chip__email">{ email.clone() }</span> },
                None => html! {},
            };
            html! {
                <div class="user-chip">
                    <span class="user-chip__avatar">{ initials(&user.name) }</span>
                    <span class="user-chip__meta">
                        <span class="user-chip__name">{ user.name.clone() }</span>
                        { email }
                    </span>
                    <button class="user-chip__signout" onclick={on_signout}>{ "Sign out" }</button>
                </div>
            }
        }
        None => html! {},
    };

    html! {
        <header class="app-bar">
            <div class="app-bar__inner">
                <a class="app-bar__brand" href={util::nav_href("/admin")}>
                    <img
                        src="https://cdn.sierrasoftworks.com/logos/icon.svg"
                        alt="The Sierra Softworks logo."
                    />
                    <span class="app-bar__brand-name">{ "Automate" }</span>
                </a>
                { nav }
                <div class="app-bar__spacer" />
                { user }
            </div>
        </header>
    }
}
