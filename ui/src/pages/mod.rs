//! Top-level routed pages.

mod activity;
mod admin;
mod auth_callback;
mod connections;
#[cfg(debug_assertions)]
mod demo;
mod kv;
mod landing;
mod login;
mod not_found;
mod protected;
mod queue;
mod workflows;

pub use activity::Activity;
pub use admin::Admin;
pub use auth_callback::AuthCallback;
pub use connections::Connections;
#[cfg(debug_assertions)]
pub use demo::DemoControls;
pub use landing::Landing;
pub use login::Login;
pub use not_found::NotFound;
pub use protected::Protected;
pub use workflows::Workflows;
