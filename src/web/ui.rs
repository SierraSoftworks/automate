use actix_web::HttpResponseBuilder;
use yew::{ServerRenderer, prelude::*};

use crate::prelude::*;

pub async fn index() -> actix_web::HttpResponse {
    let renderer = ServerRenderer::<crate::ui::Page>::with_props(|| crate::ui::PageProps {
        title: Some("Automate | Sierra Softworks".to_string()),
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

pub async fn admin_index<S: Services>(
    _services: actix_web::web::Data<S>,
) -> actix_web::HttpResponse {
    let renderer = ServerRenderer::<crate::ui::Page>::with_props(|| crate::ui::PageProps {
        title: Some("Admin | Automate".to_string()),
        children: html! {
            <crate::ui::Center>
                <h1>{ "Admin Dashboard" }</h1>
                <p>{ "Welcome to the admin dashboard." }</p>
            </crate::ui::Center>
        },
    });

    let rendered = renderer.render().await;

    actix_web::HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(format!("<!DOCTYPE html>{}", rendered))
}

pub async fn not_found() -> actix_web::HttpResponse {
    error_page(404, "Not Found", "The page you are looking for does not exist.").await
}

pub async fn error_page(code: u16, title: impl ToString, message: impl ToString) -> actix_web::HttpResponse {
    let title = title.to_string();
    let message = message.to_string();

    let renderer = ServerRenderer::<crate::ui::Page>::with_props(move || crate::ui::PageProps {
        title: Some(format!("{} | Automate", title)),
        children: html! {
            <crate::ui::Center>
                <h1><strong>{ code }</strong> { title }</h1>
                <p>{ message }</p>
            </crate::ui::Center>
        },
    });

    let rendered = renderer.render().await;

    HttpResponseBuilder::new(
        actix_web::http::StatusCode::from_u16(code).unwrap_or(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR),
    )
        .content_type("text/html; charset=utf-8")
        .body(format!("<!DOCTYPE html>{}", rendered))
}