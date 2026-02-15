use yew::prelude::*;

use crate::{prelude::*, ui::render_page};

pub async fn admin_index<S: Services>(
    _services: actix_web::web::Data<S>,
) -> actix_web::HttpResponse {
    render_page("Admin | Automate", || {
        html! {
            <crate::ui::Center>
                <h1>{ "Admin Dashboard" }</h1>
                <p>{ "Welcome to the admin dashboard." }</p>
            </crate::ui::Center>
        }
    })
    .await
}
