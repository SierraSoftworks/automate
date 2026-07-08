//! Shared data-transfer types for the Automate REST API.
//!
//! This crate is deliberately free of any web-framework, database, or UI
//! dependencies so that it can be compiled both by the `automate` agent
//! (native) and the `automate-ui` crate (WebAssembly). It defines the JSON
//! contract exchanged over the `/api/v1` endpoints.

mod kv;
mod queue;
mod user;

pub use kv::KeyValueEntry;
pub use queue::{QueueMessage, QueueStatus};
pub use user::AdminUser;
