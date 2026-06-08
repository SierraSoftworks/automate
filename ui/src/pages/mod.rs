//! Top-level routed pages.

mod dashboard;
mod db;
mod landing;
mod login;
mod not_found;
mod protected;
mod queue;

pub use dashboard::Dashboard;
pub use db::Db;
pub use landing::Landing;
pub use login::Login;
pub use not_found::NotFound;
pub use protected::Protected;
pub use queue::Queue;
