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
    let queue_partitions = services.queue().partitions().await.unwrap_or_default();

    render_page("Admin | Automate", move || {
        html! {
            <div class="admin-content">
                <h1>{ "Admin " }<strong>{ "Dashboard" }</strong></h1>
                <p class="admin-intro">
                    { "All endpoints require a request to originate from an address permitted by the " }
                    <code>{ "web.admin_acl" }</code>
                    { " filter." }
                </p>

                <div class="partition-section">
                    <h2>{ "Key-Value Store" }</h2>
                    <div class="partition-list">
                        <a class="partition-item" href="/admin/db">
                            <span class="partition-name">{ "All partitions" }</span>
                            <span class="partition-action">{ "view" }</span>
                        </a>
                    </div>
                </div>

                <div class="partition-section">
                    <h2>{ "Queue Partitions" }</h2>
                    {
                        if queue_partitions.is_empty() {
                            html! { <p class="partition-empty">{ "No queue partitions found." }</p> }
                        } else {
                            html! {
                                <div class="partition-list">
                                    { for queue_partitions.iter().map(|p| {
                                        let href = format!("/admin/queue/{p}");
                                        html! {
                                            <a class="partition-item" href={href}>
                                                <span class="partition-name">{ p }</span>
                                                <span class="partition-action">{ "view" }</span>
                                            </a>
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

pub async fn admin_db_overview<S: Services>(
    services: web::Data<S>,
) -> actix_web::HttpResponse {
    let all_entries = match services
        .kv()
        .scan::<serde_json::Value>()
        .await
    {
        Ok(entries) => entries,
        Err(err) => {
            let message = err.to_string();
            return render_page("DB | Admin | Automate", move || {
                html! {
                    <crate::ui::Center>
                        <h1>{ "Failed to scan key-value store" }</h1>
                        <p>{ message.clone() }</p>
                    </crate::ui::Center>
                }
            })
            .await;
        }
    };

    let mut groups: std::collections::BTreeMap<String, Vec<(String, serde_json::Value)>> =
        std::collections::BTreeMap::new();
    for (partition, key, value) in all_entries {
        groups.entry(partition).or_default().push((key, value));
    }
    let partitions: Vec<(String, Vec<(String, serde_json::Value)>)> =
        groups.into_iter().collect();

    render_page("DB | Admin | Automate", move || {
        html! {
            <div class="admin-content">
                {
                    if partitions.is_empty() {
                        html! {
                            <crate::ui::Center>
                                <p>{ "The key-value store is empty." }</p>
                            </crate::ui::Center>
                        }
                    } else {
                        html! {
                            <div class="kv-overview">
                                { for partitions.iter().map(|(partition, entries)| {
                                    html! {
                                        <crate::ui::KeyValueView
                                            partition={partition.clone()}
                                            entries={entries.clone()}
                                        />
                                    }
                                }) }
                            </div>
                        }
                    }
                }
            </div>
        }
    })
    .await
}

pub async fn admin_queue_partition<S: Services>(
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
