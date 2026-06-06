//! Reusable presentational components ported from the original server-rendered
//! admin UI. All text is rendered through Yew's `{value}` interpolation, which
//! HTML-escapes its content; JSON payloads are pretty-printed and rendered as
//! plain text nodes inside `<pre><code>` so that untrusted payload data can
//! never inject markup.

mod card;
mod entity;
mod helpers;
mod kv;
mod layout;
mod page_header;
mod queue;

pub use card::Card;
pub use entity::{DbEntity, EntityMetadata, Partition};
pub use helpers::Center;
pub use kv::KeyValueView;
pub use layout::Layout;
pub use page_header::PageHeader;
pub use queue::{QueueMessageDisplay, QueueView};
