use actix_web::web;
use yew::prelude::*;

use crate::{db::{KeyValueStore, Queue}, prelude::*, ui::render_page};

fn relative_time(dt: chrono::DateTime<chrono::Utc>) -> String {
    let secs = dt.signed_duration_since(chrono::Utc::now()).num_seconds();
    let abs = secs.unsigned_abs();

    let (value, unit) = if abs < 60 {
        (abs, "second")
    } else if abs < 3600 {
        (abs / 60, "minute")
    } else if abs < 86400 {
        (abs / 3600, "hour")
    } else {
        (abs / 86400, "day")
    };

    let plural = if value == 1 { "" } else { "s" };
    if secs < 0 {
        format!("{value} {unit}{plural} ago")
    } else {
        format!("in {value} {unit}{plural}")
    }
}

pub async fn admin_index<S: Services>(
    services: web::Data<S>,
) -> actix_web::HttpResponse {
    let kv_partitions = services.kv().partitions().await.unwrap_or_default();
    let queue_partitions = services.queue().partitions().await.unwrap_or_default();

    let kv_set: std::collections::BTreeSet<String> = kv_partitions.into_iter().collect();
    let queue_set: std::collections::BTreeSet<String> = queue_partitions.into_iter().collect();
    let all_partitions: Vec<String> = kv_set.union(&queue_set).cloned().collect();

    render_page("Admin | Automate", move || {
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
                    <div class="admin-endpoint">
                        <div class="admin-endpoint-method">{ "GET" }</div>
                        <div class="admin-endpoint-detail">
                            <div class="admin-endpoint-path">{ "/admin/db/" }<em>{ "{partition}" }</em>{ "/messages" }</div>
                            <div class="admin-endpoint-desc">
                                { "Shows up to 100 queued messages in the named queue partition, including
                                   each message's status (Pending, Delayed, or Reserved), scheduled time,
                                   availability window, traceparent, and JSON payload. For example " }
                                <code>{ "/admin/db/github_notifications/messages" }</code>{ "." }
                            </div>
                        </div>
                    </div>
                </div>

                <div class="partition-section">
                    <h2>{ "Partitions" }</h2>
                    {
                        if all_partitions.is_empty() {
                            html! { <p class="partition-empty">{ "No partitions found." }</p> }
                        } else {
                            html! {
                                <div class="partition-list">
                                    { for all_partitions.iter().map(|p| {
                                        let has_kv = kv_set.contains(p);
                                        let has_queue = queue_set.contains(p);
                                        let kv_href = format!("/admin/db/{p}/keys");
                                        let queue_href = format!("/admin/db/{p}/messages");
                                        html! {
                                            <div class="partition-item">
                                                <span class="partition-name">{ p }</span>
                                                <span class="partition-actions">
                                                    { if has_kv { html! {
                                                        <a class="partition-action" href={kv_href}>{ "keys" }</a>
                                                    }} else { html! {} } }
                                                    { if has_queue { html! {
                                                        <a class="partition-action" href={queue_href}>{ "messages" }</a>
                                                    }} else { html! {} } }
                                                </span>
                                            </div>
                                        }
                                    }) }
                                </div>
                            }
                        }
                    }
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

pub async fn admin_queue_partition_messages<S: Services>(
    services: web::Data<S>,
    partition: web::Path<String>,
) -> actix_web::HttpResponse {
    let partition_name = partition.into_inner();

    let messages = match services
        .queue()
        .peek::<_, serde_json::Value>(partition_name.clone(), 100)
        .await
    {
        Ok(msgs) => msgs,
        Err(err) => {
            let message = err.to_string();
            return render_page(
                format!("{partition_name} | Queue | Admin | Automate"),
                move || {
                    html! {
                        <crate::ui::Center>
                            <h1>{ "Failed to load queue" }</h1>
                            <p>{ message.clone() }</p>
                        </crate::ui::Center>
                    }
                },
            )
            .await;
        }
    };

    let now = chrono::Utc::now();
    let display: Vec<crate::ui::QueueMessageDisplay> = messages
        .into_iter()
        .map(|msg| {
            let status = if msg.reserved_by.is_some() {
                "Reserved"
            } else if msg.hidden_until > now {
                "Delayed"
            } else {
                "Pending"
            };

            let show_hidden = status != "Pending";

            crate::ui::QueueMessageDisplay {
                key: msg.key,
                payload: msg.payload,
                status: status.to_string(),
                scheduled_at_abs: msg.scheduled_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                scheduled_at_rel: relative_time(msg.scheduled_at),
                hidden_until_abs: show_hidden
                    .then(|| msg.hidden_until.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
                hidden_until_rel: show_hidden.then(|| relative_time(msg.hidden_until)),
                traceparent: msg.traceparent,
            }
        })
        .collect();

    let title = format!("{partition_name} | Queue | Admin | Automate");
    render_page(title, move || {
        html! {
            <div class="admin-content">
                <crate::ui::QueueView
                    partition={partition_name.clone()}
                    messages={display.clone()}
                />
            </div>
        }
    })
    .await
}
