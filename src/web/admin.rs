use actix_web::web;
use yew::prelude::*;

use crate::{db::KeyValueStore, prelude::*, ui::render_page};

pub async fn admin_index<S: Services>(
    _services: web::Data<S>,
) -> actix_web::HttpResponse {
    render_page("Admin | Automate", || {
        html! {
            <div class="admin-content">
                <h1>{ "Admin " }<strong>{ "Dashboard" }</strong></h1>
                <p class="admin-intro">
                    { "The following admin endpoints are available. All endpoints require a request
                       to originate from an address permitted by the " }
                    <code>{ "web.admin_acl" }</code>
                    { " filter." }
                </p>
                <div class="admin-endpoints">
                    <div class="admin-endpoint">
                        <div class="admin-endpoint-method">{ "GET" }</div>
                        <div class="admin-endpoint-detail">
                            <div class="admin-endpoint-path">{ "/admin/db/" }<em>{ "{partition}" }</em>{ "/keys" }</div>
                            <div class="admin-endpoint-desc">
                                { "Lists every row key and its JSON value stored in the named key-value
                                   partition. Replace " }<em>{ "{partition}" }</em>
                                { " with the exact partition name used by the application, for example " }
                                <code>{ "/admin/db/github_notifications/keys" }</code>{ "." }
                            </div>
                        </div>
                    </div>
                </div>
            </div>
        }
    })
    .await
}

pub async fn admin_db_partition_keys<S: Services>(
    services: web::Data<S>,
    partition: web::Path<String>,
) -> actix_web::HttpResponse {
    let partition_name = partition.into_inner();

    let entries: Vec<(String, serde_json::Value)> = match services
        .kv()
        .list::<serde_json::Value>(partition_name.clone())
        .await
    {
        Ok(entries) => entries,
        Err(err) => {
            let message = err.to_string();
            return render_page(
                format!("{partition_name} | DB | Admin | Automate"),
                move || {
                    html! {
                        <crate::ui::Center>
                            <h1>{ "Failed to load partition" }</h1>
                            <p>{ message.clone() }</p>
                        </crate::ui::Center>
                    }
                },
            )
            .await;
        }
    };

    let title = format!("{partition_name} | DB | Admin | Automate");
    render_page(title, move || {
        html! {
            <div class="admin-content">
                <crate::ui::KeyValueView
                    partition={partition_name.clone()}
                    entries={entries.clone()}
                />
            </div>
        }
    })
    .await
}
