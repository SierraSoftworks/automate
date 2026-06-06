use automate_api::{QueueMessage, QueueStatus};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::app::AuthHandle;
use crate::components::{PageHeader, QueueMessageDisplay, QueueView};
use crate::fixtures;
use crate::util;

use super::Protected;

enum Load {
    Loading,
    Ready(Vec<QueueMessage>),
    Failed(String),
}

fn to_display(msg: &QueueMessage) -> QueueMessageDisplay {
    let status = match msg.status {
        QueueStatus::Pending => "Pending",
        QueueStatus::Reserved => "Reserved",
        QueueStatus::Delayed => "Delayed",
    }
    .to_string();

    QueueMessageDisplay {
        partition: msg.partition.clone(),
        key: msg.key.clone(),
        payload: msg.payload.clone(),
        status,
        scheduled_at: msg.scheduled_at,
        scheduled_at_abs: util::format_abs(msg.scheduled_at),
        scheduled_at_rel: util::relative_time(msg.scheduled_at),
        hidden_until_abs: msg.hidden_until.map(util::format_abs),
        hidden_until_rel: msg.hidden_until.map(util::relative_time),
        traceparent: msg.traceparent.clone(),
    }
}

#[function_component(Queue)]
pub fn queue() -> Html {
    html! { <Protected><QueueContent /></Protected> }
}

#[function_component(QueueContent)]
fn queue_content() -> Html {
    let auth = use_context::<AuthHandle>().expect("AuthHandle context must be provided");
    let state = use_state(|| Load::Loading);
    let reload = use_state(|| 0u32);

    {
        let state = state.clone();
        use_effect_with(*reload, move |_| {
            state.set(Load::Loading);
            spawn_local(async move {
                if fixtures::is_demo() {
                    state.set(Load::Ready(fixtures::queue_messages()));
                    return;
                }
                match api::list_queue().await {
                    Ok(messages) => state.set(Load::Ready(messages)),
                    Err(error) => state.set(Load::Failed(error.to_string())),
                }
            });
            || ()
        });
    }

    let on_trigger = {
        let reload = reload.clone();
        Callback::from(move |msg: QueueMessageDisplay| {
            if fixtures::is_demo() {
                return;
            }
            let reload = reload.clone();
            spawn_local(async move {
                let _ = api::trigger_queue(&msg.partition, &msg.key, msg.payload.clone()).await;
                reload.set(*reload + 1);
            });
        })
    };

    let on_delete = {
        let reload = reload.clone();
        let state = state.clone();
        Callback::from(move |(partition, key): (String, String)| {
            if fixtures::is_demo() {
                if let Load::Ready(messages) = &*state {
                    let remaining = messages
                        .iter()
                        .filter(|m| !(m.partition == partition && m.key == key))
                        .cloned()
                        .collect();
                    state.set(Load::Ready(remaining));
                }
                return;
            }
            let reload = reload.clone();
            spawn_local(async move {
                let _ = api::delete_queue(&partition, &key).await;
                reload.set(*reload + 1);
            });
        })
    };

    let on_signout = {
        let signout = auth.signout.clone();
        Callback::from(move |_: MouseEvent| signout.emit(()))
    };

    let body = match &*state {
        Load::Loading => html! { <p class="admin-intro">{ "Loading…" }</p> },
        Load::Failed(error) => html! { <div class="error-banner">{ error.clone() }</div> },
        Load::Ready(messages) => {
            let mut display: Vec<QueueMessageDisplay> = messages.iter().map(to_display).collect();
            display.sort_by_key(|m| m.scheduled_at);
            html! {
                <QueueView
                    messages={display}
                    on_trigger={on_trigger.clone()}
                    on_delete={on_delete.clone()}
                />
            }
        }
    };

    html! {
        <div class="admin-content">
            <PageHeader
                title="Queue"
                user_name={auth.user.as_ref().map(|u| AttrValue::from(u.name.clone()))}
                user_email={auth.user.as_ref().and_then(|u| u.email.clone()).map(AttrValue::from)}
                on_signout={on_signout}
            />
            { body }
        </div>
    }
}
