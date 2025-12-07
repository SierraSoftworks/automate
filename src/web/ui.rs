use yew::{prelude::*, ServerRenderer};

pub async fn index() -> impl actix_web::Responder {
    let renderer = ServerRenderer::<crate::ui::Page>::with_props(|| crate::ui::PageProps {
        title: Some("Automate | Sierra Softworks"),
        children: html! {
            <crate::ui::Center>
                <h1>
                    <strong>{ "Automate" }</strong> { " by Sierra Softworks" }
                </h1>
                <p>
                    { "Automate various aspects of your life without needing to trust someone else with your data." }
                </p>
            </crate::ui::Center>
        },
    });

    let rendered = renderer.render().await;

    actix_web::HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(format!("<!DOCTYPE html>{}", rendered))
}

pub async fn not_found() -> impl actix_web::Responder {
    let renderer = ServerRenderer::<crate::ui::Page>::with_props(|| crate::ui::PageProps {
        title: None,
        children: html! {
            <crate::ui::Center>
                <h1><strong>{ "404" }</strong> { " Not Found" }</h1>
                <p>{ "The page you are looking for does not exist." }</p>
            </crate::ui::Center>
        },
    });

    let rendered = renderer.render().await;

    actix_web::HttpResponse::NotFound()
        .content_type("text/html; charset=utf-8")
        .body(format!("<!DOCTYPE html>{}", rendered))
}

pub async fn internal_server_error() -> impl actix_web::Responder {
    let renderer = ServerRenderer::<crate::ui::Page>::with_props(|| crate::ui::PageProps {
        title: None,
        children: html! {
            <crate::ui::Center>
                <h1><strong>{ "500" }</strong> { " Internal Server Error" }</h1>
                <p>{ "An unexpected error occurred while processing your request." }</p>
            </crate::ui::Center>
        },
    });

    let rendered = renderer.render().await;

    actix_web::HttpResponse::InternalServerError()
        .content_type("text/html; charset=utf-8")
        .body(format!("<!DOCTYPE html>{}", rendered))
}