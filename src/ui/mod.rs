use yew::{ServerRenderer, virtual_dom::VNode};

mod card;
mod csrf;
mod db;
mod entity;
mod helpers;
mod page;
mod page_header;

pub use card::Card;
pub use csrf::CsrfToken;
pub use db::{KeyValueView, QueueMessageDisplay, QueueView};
pub use entity::{DbEntity, EntityMetadata, Partition};
pub use helpers::*;
pub use page::*;
pub use page_header::PageHeader;

pub async fn render_page<F>(title: impl ToString, children: F) -> actix_web::HttpResponse
where
    F: Fn() -> VNode + 'static + Send,
{
    let title = title.to_string();
    let renderer = ServerRenderer::<Page>::with_props(move || PageProps {
        title: Some(title.clone()),
        children: children(),
    });
    actix_web::HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(renderer.render().await)
}
