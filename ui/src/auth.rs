//! Browser-side helpers for the server-driven OIDC session.
//!
//! Authentication is handled entirely by the agent: there are no tokens or PKCE
//! material in the browser. Signing in is a full-page navigation to the agent's
//! login endpoint (which redirects to the identity provider and back, setting an
//! `HttpOnly` session cookie). Signing out posts to the logout endpoint and then
//! returns to the app root. When the session's ID token lapses the agent can
//! renew it from an `HttpOnly` refresh-token cookie, so [`refresh_session`] just
//! POSTs to the renewal endpoint and the browser picks up the re-issued cookies.

use futures::FutureExt;

use crate::api;
use single_flight::SingleFlight;

/// A single-flight coordinator: it collapses concurrent runs of an async
/// operation onto one shared execution so the operation runs at most once at a
/// time, no matter how many callers ask for it together (see [`refresh_session`]
/// for why that matters). Ported from SierraSoftworks/grey. Browser-only — it
/// stores its state in an `Rc`/`RefCell`, so it lives behind a `thread_local`
/// rather than a lock.
mod single_flight {
    use std::cell::{Cell, RefCell};
    use std::future::Future;
    use std::rc::Rc;

    use futures::FutureExt;
    use futures::future::{LocalBoxFuture, Shared};

    struct Inner<T: Clone> {
        /// The run currently in flight, tagged with the generation that started
        /// it; cloned for every caller that joins it and cleared once it settles,
        /// so the next call starts a fresh run.
        current: RefCell<Option<(u64, Shared<LocalBoxFuture<'static, T>>)>>,
        generation: Cell<u64>,
    }

    pub struct SingleFlight<T: Clone> {
        inner: Rc<Inner<T>>,
    }

    impl<T: Clone + 'static> SingleFlight<T> {
        pub fn new() -> Self {
            Self {
                inner: Rc::new(Inner {
                    current: RefCell::new(None),
                    generation: Cell::new(0),
                }),
            }
        }

        /// Runs the future produced by `start`, unless a run begun by an earlier,
        /// still-unsettled call is already in flight — in which case that one is
        /// joined instead and `start` is never invoked. The returned future is
        /// self-contained (it owns a handle to the shared state), so it can be
        /// awaited outside the borrow that produced it.
        pub fn run<F>(&self, start: F) -> impl Future<Output = T> + use<F, T>
        where
            F: FnOnce() -> LocalBoxFuture<'static, T>,
        {
            let inner = self.inner.clone();
            async move {
                let (generation, shared) = {
                    let mut current = inner.current.borrow_mut();
                    if let Some((generation, existing)) = current.as_ref() {
                        (*generation, existing.clone())
                    } else {
                        let generation = inner.generation.get().wrapping_add(1);
                        inner.generation.set(generation);
                        let shared = start().shared();
                        *current = Some((generation, shared.clone()));
                        (generation, shared)
                    }
                };

                let result = shared.await;

                // Retire this run so the next call starts afresh, but only if a
                // later run hasn't already replaced it — otherwise we'd strand
                // the newer one and let a second concurrent run start.
                let mut current = inner.current.borrow_mut();
                if matches!(current.as_ref(), Some((g, _)) if *g == generation) {
                    *current = None;
                }
                result
            }
        }
    }
}

thread_local! {
    /// Coalesces concurrent, 401-driven renewals onto a single redemption of the
    /// refresh token (see [`refresh_session`]).
    static REFRESH: SingleFlight<Result<(), ()>> = SingleFlight::new();
}

/// Renews the server-held session from its refresh-token cookie.
///
/// Concurrent callers are coalesced onto a single renewal: the first starts the
/// refresh and the rest await its result. This matters because providers may
/// *rotate* the refresh token — each redemption can invalidate the token it used.
/// Several requests routinely fail together (a page's parallel fetches all hit a
/// 401 the moment the ID token lapses); were each to redeem the refresh token
/// independently, only the first would succeed and the losers would kill the
/// freshly renewed session. `Ok` means the agent re-issued the session cookie and
/// the failed request can be retried.
pub async fn refresh_session() -> Result<(), ()> {
    REFRESH
        .with(|flight| flight.run(|| do_refresh_session().boxed_local()))
        .await
}

async fn do_refresh_session() -> Result<(), ()> {
    let response = gloo_net::http::Request::post("/api/v1/auth/refresh")
        .send()
        .await
        .map_err(|_| ())?;
    if response.ok() { Ok(()) } else { Err(()) }
}

/// Returns the browser window, panicking if it is somehow unavailable (it always
/// is in a wasm context).
fn window() -> web_sys::Window {
    web_sys::window().expect("a browser window should be available")
}

/// The current path + query + hash, used as the post-login destination so the
/// user returns to where they were after authenticating.
fn current_path() -> String {
    let location = window().location();
    let path = location.pathname().unwrap_or_else(|_| "/".to_string());
    let search = location.search().unwrap_or_default();
    let hash = location.hash().unwrap_or_default();
    format!("{path}{search}{hash}")
}

/// Begins the login flow by navigating the browser to the agent's login
/// endpoint, preserving the current location as the post-login destination.
pub fn begin_login() {
    begin_login_to(&current_path());
}

/// Begins the login flow, returning the user to `return_to` once they have
/// authenticated. Used to send a visitor straight into the OIDC flow with the
/// admin area as their destination, skipping an intermediate sign-in screen.
pub fn begin_login_to(return_to: &str) {
    let encoded: String = js_sys::encode_uri_component(return_to).into();
    let url = format!("/api/v1/auth/login?return_to={encoded}");
    let _ = window().location().set_href(&url);
}

/// Signs the user out by clearing the server session, then returns to the app
/// root.
pub async fn logout() {
    let _ = api::logout().await;
    let _ = window().location().set_href("/");
}
