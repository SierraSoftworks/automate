//! The address a webhook-triggered workflow is reached at.
//!
//! Shown in full rather than kept back. Unlike a connection's credential this
//! one has to be readable — its owner has to paste it into the service that
//! will call it — and a URL you can only see once is a URL that gets written
//! down somewhere worse than here.

use yew::prelude::*;

use crate::api;
use crate::components::{Button, ButtonKind};

#[derive(Properties, PartialEq)]
pub struct WebhookAddressProps {
    /// The workflow this address belongs to.
    pub workflow: AttrValue,

    /// The path the agent reported, which the browser completes into a URL.
    pub path: AttrValue,

    /// Invoked once a new address has been issued.
    pub on_rotated: Callback<()>,
}

#[function_component(WebhookAddress)]
pub fn webhook_address(props: &WebhookAddressProps) -> Html {
    let copied = use_state(|| false);
    let confirming = use_state(|| false);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    // Completed here rather than by the agent, which behind a proxy does not
    // reliably know the address it is reached on. The browser is already talking
    // to the right one.
    let url = origin()
        .map(|origin| format!("{origin}{}", props.path))
        .unwrap_or_else(|| props.path.to_string());

    let on_copy = {
        let (url, copied) = (url.clone(), copied.clone());

        Callback::from(move |_| {
            let (url, copied) = (url.clone(), copied.clone());

            wasm_bindgen_futures::spawn_local(async move {
                if copy_to_clipboard(&url).await {
                    copied.set(true);
                }
            });
        })
    };

    let on_ask = {
        let confirming = confirming.clone();
        Callback::from(move |_| confirming.set(true))
    };

    let on_cancel = {
        let confirming = confirming.clone();
        Callback::from(move |_| confirming.set(false))
    };

    let on_rotate = {
        let (workflow, busy, error, confirming, on_rotated) = (
            props.workflow.to_string(),
            busy.clone(),
            error.clone(),
            confirming.clone(),
            props.on_rotated.clone(),
        );

        Callback::from(move |_| {
            let (workflow, busy, error, confirming, on_rotated) = (
                workflow.clone(),
                busy.clone(),
                error.clone(),
                confirming.clone(),
                on_rotated.clone(),
            );

            wasm_bindgen_futures::spawn_local(async move {
                busy.set(true);
                error.set(None);

                match api::rotate_webhook(&workflow).await {
                    Ok(_) => {
                        confirming.set(false);
                        on_rotated.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }

                busy.set(false);
            });
        })
    };

    html! {
        <div class="webhook-address">
            <span class="webhook-address__label">{ "Send deliveries to" }</span>

            <div class="webhook-address__row">
                // Read-only rather than disabled, so it can still be selected and
                // copied by hand when the clipboard is not available to us.
                <input
                    class="webhook-address__url"
                    type="text"
                    readonly={true}
                    value={url.clone()}
                />

                <Button kind={ButtonKind::Subtle} onclick={on_copy}>
                    if *copied { { "Copied" } } else { { "Copy" } }
                </Button>
            </div>

            if *confirming {
                <div class="webhook-address__confirm">
                    <p class="webhook-address__warning">
                        { "A new address takes effect at once, and this one stops working. \
                           Anything already sending here will need updating." }
                    </p>

                    <div class="webhook-address__actions">
                        <Button kind={ButtonKind::Danger} onclick={on_rotate} busy={*busy}>
                            { "Issue a new address" }
                        </Button>
                        <Button kind={ButtonKind::Subtle} onclick={on_cancel} disabled={*busy}>
                            { "Keep this one" }
                        </Button>
                    </div>
                </div>
            } else {
                <button class="webhook-address__rotate" onclick={on_ask}>
                    { "Issue a new address" }
                </button>
            }

            if let Some(message) = (*error).clone() {
                <p class="webhook-address__error">{ message }</p>
            }
        </div>
    }
}

/// Where the browser is talking to, so a path can be shown as a URL.
fn origin() -> Option<String> {
    web_sys::window()?.location().origin().ok()
}

/// Puts the address on the clipboard, reporting whether it got there.
///
/// The clipboard is unavailable over plain HTTP and when permission is refused,
/// which is why the address is also selectable: failing here costs the
/// convenience, not the ability to get the URL.
async fn copy_to_clipboard(value: &str) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };

    let promise = window.navigator().clipboard().write_text(value);
    wasm_bindgen_futures::JsFuture::from(promise).await.is_ok()
}
