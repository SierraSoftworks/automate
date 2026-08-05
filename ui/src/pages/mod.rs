//! Top-level routed pages.

mod admin;
mod auth_callback;
mod connections;
mod kv;
mod landing;
mod login;
mod not_found;
mod protected;
mod queue;
mod workflows;

pub use admin::Admin;
pub use auth_callback::AuthCallback;
pub use connections::Connections;
pub use workflows::Workflows;
pub use landing::Landing;
pub use login::Login;
pub use not_found::NotFound;
pub use protected::Protected;
