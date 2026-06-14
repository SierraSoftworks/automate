//! Application root: routing, the authentication gate, and the shared auth
//! context consumed by the individual pages.

use automate_api::AdminUser;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew_router::prelude::*;

use crate::api::{self, ApiError};
use crate::auth;
use crate::components::AdminShell;
use crate::fixtures;
use crate::pages;

/// The client-side routes handled by the SPA.
#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    /// The public landing page.
    #[at("/")]
    Landing,
    /// The admin dashboard (also serves the bare `/admin/` path).
    #[at("/admin")]
    AdminRoot,
    #[at("/admin/")]
    Dashboard,
    #[at("/admin/db")]
    Db,
    #[at("/admin/queue")]
    Queue,
    #[not_found]
    #[at("/404")]
    NotFound,
}

/// The resolved authentication state of the application.
#[derive(Clone, PartialEq)]
pub enum AuthStatus {
    /// The configuration is still being resolved.
    Loading,
    /// OIDC is not configured; the API is reachable without signing in (gated by
    /// the server-side ACL only).
    Disabled,
    /// A user is signed in (or demo mode is active).
    SignedIn(AdminUser),
    /// Authentication is required; the browser must start the login flow.
    NeedsLogin,
    /// Access was refused by the admin ACL. Signing in cannot change the outcome
    /// (and, when OIDC is disabled, is not possible), so the UI must not offer it.
    Forbidden,
    /// Resolving the authentication state failed.
    Error(String),
}

/// The shared authentication handle provided to every page via context.
#[derive(Clone, PartialEq)]
pub struct AuthHandle {
    pub status: AuthStatus,
    pub user: Option<AdminUser>,
    pub login: Callback<()>,
    pub signout: Callback<()>,
}

/// Resolves the authentication state once on mount and exposes login/sign-out
/// actions.
#[hook]
fn use_auth() -> AuthHandle {
    let status = use_state(|| AuthStatus::Loading);

    {
        let status = status.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if fixtures::is_demo() {
                    status.set(AuthStatus::SignedIn(fixtures::admin_user()));
                    return;
                }

                // Probe the protected API: a signed-in user comes back as the
                // identity, an unauthenticated request as `401` (which starts the
                // login flow), and a request that needs no sign-in (OIDC
                // disabled) as `204`.
                match api::me().await {
                    Ok(Some(user)) => status.set(AuthStatus::SignedIn(user)),
                    Ok(None) => status.set(AuthStatus::Disabled),
                    Err(ApiError::Unauthorized) => status.set(AuthStatus::NeedsLogin),
                    Err(ApiError::Forbidden) => status.set(AuthStatus::Forbidden),
                    Err(e) => status.set(AuthStatus::Error(e.to_string())),
                }
            });
            || ()
        });
    }

    let login = Callback::from(move |_| auth::begin_login());

    let signout = Callback::from(move |_| {
        spawn_local(async move {
            auth::logout().await;
        });
    });

    let user = match &*status {
        AuthStatus::SignedIn(user) => Some(user.clone()),
        _ => None,
    };

    AuthHandle {
        status: (*status).clone(),
        user,
        login,
        signout,
    }
}

#[function_component(App)]
pub fn app() -> Html {
    html! {
        <BrowserRouter>
            <AppInner />
        </BrowserRouter>
    }
}

#[function_component(AppInner)]
fn app_inner() -> Html {
    let auth = use_auth();
    html! {
        <ContextProvider<AuthHandle> context={auth}>
            <Switch<Route> render={switch} />
        </ContextProvider<AuthHandle>>
    }
}

fn switch(route: Route) -> Html {
    match route {
        Route::Landing => html! { <pages::Landing /> },
        Route::AdminRoot | Route::Dashboard => html! {
            <AdminShell><pages::Dashboard /></AdminShell>
        },
        Route::Db => html! { <AdminShell><pages::Db /></AdminShell> },
        Route::Queue => html! { <AdminShell><pages::Queue /></AdminShell> },
        Route::NotFound => html! { <pages::NotFound /> },
    }
}
