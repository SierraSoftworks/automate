//! Reusable presentational components ported from the original server-rendered
//! admin UI. All text is rendered through Yew's `{value}` interpolation, which
//! HTML-escapes its content; JSON payloads are pretty-printed and rendered as
//! plain text nodes inside `<pre><code>` so that untrusted payload data can
//! never inject markup.

mod admin_shell;
mod alert;
mod app_bar;
mod connect_menu;
mod entity;
mod helpers;
mod json_highlight;
mod layout;
mod page_title;
mod partition_browser;
mod refresh_button;

pub use admin_shell::{AdminShell, PageActions};
pub use alert::{Alert, AlertKind};
pub use app_bar::AppBar;
pub use connect_menu::ConnectMenu;
pub use entity::{DbEntity, EntityMetadata};
pub use helpers::Center;
pub use json_highlight::JsonHighlight;
pub use layout::Layout;
pub use page_title::PageTitle;
pub use partition_browser::{BrowserEntry, BrowserPartition, PartitionBrowser};
pub use refresh_button::RefreshButton;
