use yew::{ServerRenderer, virtual_dom::VNode};

mod helpers;
mod page;

pub use helpers::*;
pub use page::*;

pub async fn render_page<F>(title: impl ToString, children: F) -> actix_web::HttpResponse
where
    F: Fn() -> VNode + 'static + Send,{
    let title = title.to_string();
    let renderer = ServerRenderer::<Page>::with_props(move || PageProps {
        title: Some(title.clone()),
        children: children(),
    });
    actix_web::HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(renderer.render().await)
}
