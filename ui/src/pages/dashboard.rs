use yew::prelude::*;

use crate::components::Card;
use crate::util;

#[function_component(Dashboard)]
pub fn dashboard() -> Html {
    let kv_icon = html! {
        <svg
            viewBox="0 0 24 24"
            width="28"
            height="28"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
        >
            <ellipse cx="12" cy="5" rx="9" ry="3" />
            <path d="M21 5v6c0 1.66-4 3-9 3s-9-1.34-9-3V5" />
            <path d="M21 11v6c0 1.66-4 3-9 3s-9-1.34-9-3v-6" />
        </svg>
    };

    let queue_icon = html! {
        <svg
            viewBox="0 0 24 24"
            width="28"
            height="28"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
        >
            <line x1="8" y1="6" x2="21" y2="6" />
            <line x1="8" y1="12" x2="21" y2="12" />
            <line x1="8" y1="18" x2="21" y2="18" />
            <circle cx="3.5" cy="6" r="1" />
            <circle cx="3.5" cy="12" r="1" />
            <circle cx="3.5" cy="18" r="1" />
        </svg>
    };

    html! {
        <div class="admin-cards">
            <Card
                href={util::nav_href("/admin/db")}
                title="Key-Value Store"
                description="Browse and manage the persistent key-value state used by collectors and publishers."
                icon={kv_icon}
            />
            <Card
                href={util::nav_href("/admin/queue")}
                title="Queue"
                description="Inspect pending jobs, trigger them on demand, or remove them from their queues."
                icon={queue_icon}
            />
        </div>
    }
}
