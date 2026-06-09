//! Top-level routed pages.

mod admin;
mod kv;
mod landing;
mod login;
mod not_found;
mod protected;
mod queue;

pub use admin::Admin;
pub use landing::Landing;
pub use login::Login;
pub use not_found::NotFound;
pub use protected::Protected;
