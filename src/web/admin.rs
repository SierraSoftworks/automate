use actix_web::HttpMessage;
use actix_web::web;
use yew::prelude::*;

use crate::{
    db::{KeyValueStore, Queue},
    prelude::*,
    ui::render_page,
};

use super::helpers::oidc::AdminUser;

/// Extracts the signed-in user's display name and email (if any) from the
/// request extensions populated by the admin authentication middleware.
fn admin_user(req: &actix_web::HttpRequest) -> (Option<String>, Option<String>) {
    match req.extensions().get::<AdminUser>() {
        Some(user) => (Some(user.name.clone()), user.email.clone()),
        None => (None, None),
    }
}

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
    req: actix_web::HttpRequest,
    _services: web::Data<S>,
) -> actix_web::HttpResponse {
    let (user_name, user_email) = admin_user(&req);
    render_page("Admin | Automate", move || {
        html! {
            <div class="admin-content">
                <crate::ui::AdminHeader
                    title="Dashboard"
                    show_back={false}
                    user_name={user_name.clone().map(AttrValue::from)}
                    user_email={user_email.clone().map(AttrValue::from)}
                />
                <p class="admin-intro">
                    { "All endpoints require a request to originate from an address permitted by the " }
                    <code>{ "web.admin.acl" }</code>
                    { " filter." }
                </p>

                <div class="admin-cards">
                    <a class="admin-card" href="/admin/db">
                        <span class="admin-card-icon">
                            <svg
                                viewBox="0 0 24 24"
                                width="32"
                                height="32"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2"
                                stroke-linecap="round"
                                stroke-linejoin="round"
                                aria-hidden="true"
                            >
                                <ellipse cx="12" cy="5" rx="9" ry="3" />
                                <path d="M3 5v14c0 1.66 4.03 3 9 3s9-1.34 9-3V5" />
                                <path d="M3 12c0 1.66 4.03 3 9 3s9-1.34 9-3" />
                            </svg>
                        </span>
                        <span class="admin-card-body">
                            <span class="admin-card-title">{ "Key-Value Store" }</span>
                            <span class="admin-card-desc">
                                { "Inspect and manage persisted key-value partitions." }
                            </span>
                        </span>
                    </a>

                    <a class="admin-card" href="/admin/queue">
                        <span class="admin-card-icon">
                            <svg
                                viewBox="0 0 24 24"
                                width="32"
                                height="32"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2"
                                stroke-linecap="round"
                                stroke-linejoin="round"
                                aria-hidden="true"
                            >
                                <line x1="3" y1="6" x2="21" y2="6" />
                                <line x1="3" y1="12" x2="21" y2="12" />
                                <line x1="3" y1="18" x2="21" y2="18" />
                                <circle cx="6" cy="6" r="1.4" fill="currentColor" stroke="none" />
                                <circle cx="6" cy="12" r="1.4" fill="currentColor" stroke="none" />
                                <circle cx="6" cy="18" r="1.4" fill="currentColor" stroke="none" />
                            </svg>
                        </span>
                        <span class="admin-card-body">
                            <span class="admin-card-title">{ "Queue" }</span>
                            <span class="admin-card-desc">
                                { "Review, trigger, and delete queued job messages." }
                            </span>
                        </span>
                    </a>
                </div>
            </div>
        }
    })
    .await
}

pub async fn admin_db_overview<S: Services>(
    req: actix_web::HttpRequest,
    services: web::Data<S>,
) -> actix_web::HttpResponse {
    let all_entries = match KeyValueStore::scan(&services.kv()).await {
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
    let partitions: Vec<(String, Vec<(String, serde_json::Value)>)> = groups.into_iter().collect();
    let csrf_token = super::helpers::csrf::generate_token(&services.config().web.admin);
    let (user_name, user_email) = admin_user(&req);

    render_page("DB | Admin | Automate", move || {
        html! {
            <div class="admin-content">
                <crate::ui::AdminHeader
                    title="Key-Value Store"
                    subtitle="Persisted key-value partitions"
                    user_name={user_name.clone().map(AttrValue::from)}
                    user_email={user_email.clone().map(AttrValue::from)}
                />
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
                                            csrf_token={csrf_token.clone()}
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

pub async fn admin_queue<S: Services>(
    req: actix_web::HttpRequest,
    services: web::Data<S>,
) -> actix_web::HttpResponse {
    let partitions = match services.queue().partitions().await {
        Ok(partitions) => partitions,
        Err(err) => {
            let message = err.to_string();
            return render_page("Queue | Admin | Automate", move || {
                html! {
                    <crate::ui::Center>
                        <h1>{ "Failed to load queue" }</h1>
                        <p>{ message.clone() }</p>
                    </crate::ui::Center>
                }
            })
            .await;
        }
    };

    let now = chrono::Utc::now();
    let mut display: Vec<crate::ui::QueueMessageDisplay> = Vec::new();

    for partition_name in partitions {
        let messages = match services
            .queue()
            .peek::<_, serde_json::Value>(partition_name.clone(), 100)
            .await
        {
            Ok(msgs) => msgs,
            Err(err) => {
                let message = err.to_string();
                return render_page("Queue | Admin | Automate", move || {
                    html! {
                        <crate::ui::Center>
                            <h1>{ "Failed to load queue" }</h1>
                            <p>{ message.clone() }</p>
                        </crate::ui::Center>
                    }
                })
                .await;
            }
        };

        display.extend(messages.into_iter().map(|msg| {
            let status = if msg.reserved_by.is_some() {
                "Reserved"
            } else if msg.hidden_until > now {
                "Delayed"
            } else {
                "Pending"
            };

            let show_hidden = status != "Pending";

            crate::ui::QueueMessageDisplay {
                partition: partition_name.clone(),
                key: msg.key,
                payload: msg.payload,
                status: status.to_string(),
                scheduled_at: msg.scheduled_at,
                scheduled_at_abs: msg.scheduled_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                scheduled_at_rel: relative_time(msg.scheduled_at),
                hidden_until_abs: show_hidden
                    .then(|| msg.hidden_until.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
                hidden_until_rel: show_hidden.then(|| relative_time(msg.hidden_until)),
                traceparent: msg.traceparent,
            }
        }));
    }

    display.sort_by_key(|msg| msg.scheduled_at);
    let csrf_token = super::helpers::csrf::generate_token(&services.config().web.admin);
    let (user_name, user_email) = admin_user(&req);

    render_page("Queue | Admin | Automate", move || {
        html! {
            <div class="admin-content">
                <crate::ui::AdminHeader
                    title="Queue"
                    subtitle="Queued job messages"
                    user_name={user_name.clone().map(AttrValue::from)}
                    user_email={user_email.clone().map(AttrValue::from)}
                />
                <crate::ui::QueueView messages={display.clone()} csrf_token={csrf_token.clone()} />
            </div>
        }
    })
    .await
}

#[derive(serde::Deserialize)]
pub struct DeleteFormData {
    pub partition: String,
    pub key: String,
    pub csrf_token: String,
}

pub async fn admin_db_delete<S: Services>(
    services: web::Data<S>,
    req: actix_web::HttpRequest,
    form: web::Form<DeleteFormData>,
) -> actix_web::HttpResponse {
    let form = form.into_inner();
    if !super::helpers::csrf::validate_token(&services.config().web.admin, &form.csrf_token) {
        return csrf_rejected();
    }
    if let Err(err) = services.kv().remove(form.partition, form.key).await {
        return actix_web::HttpResponse::InternalServerError().body(err.to_string());
    }
    redirect_back(&req)
}

#[derive(serde::Deserialize)]
pub struct TriggerFormData {
    pub partition: String,
    pub key: String,
    /// Serialised JSON payload
    pub payload: String,
    pub csrf_token: String,
}

pub async fn admin_queue_trigger<S: Services>(
    services: web::Data<S>,
    req: actix_web::HttpRequest,
    form: web::Form<TriggerFormData>,
) -> actix_web::HttpResponse {
    let form = form.into_inner();
    if !super::helpers::csrf::validate_token(&services.config().web.admin, &form.csrf_token) {
        return csrf_rejected();
    }
    let payload: serde_json::Value = match serde_json::from_str(&form.payload) {
        Ok(v) => v,
        Err(e) => {
            return actix_web::HttpResponse::BadRequest()
                .body(format!("Invalid payload JSON: {e}"));
        }
    };
    if let Err(err) = services
        .queue()
        .enqueue(form.partition, payload, Some(form.key.into()), None)
        .await
    {
        return actix_web::HttpResponse::InternalServerError().body(err.to_string());
    }
    redirect_back(&req)
}

pub async fn admin_queue_delete<S: Services>(
    services: web::Data<S>,
    req: actix_web::HttpRequest,
    form: web::Form<DeleteFormData>,
) -> actix_web::HttpResponse {
    let form = form.into_inner();
    if !super::helpers::csrf::validate_token(&services.config().web.admin, &form.csrf_token) {
        return csrf_rejected();
    }
    if let Err(err) = services.queue().purge(form.partition, form.key).await {
        return actix_web::HttpResponse::InternalServerError().body(err.to_string());
    }
    redirect_back(&req)
}

fn csrf_rejected() -> actix_web::HttpResponse {
    actix_web::HttpResponse::Forbidden()
        .body("The form submission could not be verified. Please reload the page and try again.")
}

fn redirect_back(req: &actix_web::HttpRequest) -> actix_web::HttpResponse {
    let location = req
        .headers()
        .get(actix_web::http::header::REFERER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("/admin")
        .to_owned();
    actix_web::HttpResponse::SeeOther()
        .insert_header((actix_web::http::header::LOCATION, location))
        .finish()
}
