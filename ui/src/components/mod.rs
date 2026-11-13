//! The pieces of the admin UI that are specific to Automate.
//!
//! Controls, page chrome and feedback all come from [yewi](https://yewi.sierrasoftworks.com);
//! what lives here is what yewi cannot know about: the shell that ties the
//! routes together, forms drawn from the agent's descriptions, and the data
//! browser. All text is rendered through Yew's `{value}` interpolation, which
//! HTML-escapes its content, so untrusted payload data can never inject markup.

mod admin_shell;
mod connect_menu;
mod connections_panel;
mod documentation;
pub mod dynamic_form;
mod entity;
mod filter_input;
pub mod form;
mod impersonation;
mod partition_browser;
mod refresh_button;
mod webhook_address;

pub use admin_shell::{AdminShell, Footer, PageActions, brand};
pub use connect_menu::ConnectMenu;
#[cfg_attr(not(debug_assertions), allow(unused_imports))]
pub use connections_panel::ConnectionsPanel;
pub use documentation::Documentation;
pub use dynamic_form::{DynamicForm, FetchedOptions};
pub use entity::{DbEntity, EntityMetadata};
pub use filter_input::FilterInput;
pub use impersonation::{ActAsItems, ImpersonationBanner, ImpersonationNotice};
pub use partition_browser::{BrowserEntry, BrowserPartition, PartitionBrowser};
pub use refresh_button::RefreshButton;
pub use webhook_address::WebhookAddress;
