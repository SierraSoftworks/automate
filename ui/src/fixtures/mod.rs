//! Baked-in sample data for offline UI previews.
//!
//! Appending `?demo` to the URL (for example when running `trunk serve` without
//! a backend) makes the app render fixture data instead of calling the API, so
//! the interface can be developed and reviewed without a running agent.
//!
//! The substitution happens inside [`crate::api`] rather than in the pages. A
//! page written against the real client therefore works in demo mode without
//! knowing demo mode exists, and cannot drift out of it by adding a call that
//! nobody remembered to stub.
//!
//! Demo mode is a development convenience, so everything below the flag itself
//! is compiled only into debug builds: a release bundle contains no fixtures and
//! no path that could reach them.

#[cfg(debug_assertions)]
mod data;
#[cfg(debug_assertions)]
mod store;

#[cfg(debug_assertions)]
pub use store::*;

/// Returns true when the current URL requests demo mode (`?demo`).
#[cfg(debug_assertions)]
pub fn is_demo() -> bool {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .map(|search| search.contains("demo"))
        .unwrap_or(false)
}

/// Demo mode is unavailable in release builds.
#[cfg(not(debug_assertions))]
pub fn is_demo() -> bool {
    false
}
