//! The address a webhook-triggered workflow is reached at.
//!
//! Shown in full rather than kept back. Unlike a connection's credential this
//! one has to be readable — its owner has to paste it into the service that
//! will call it — and a URL you can only see once is a URL that gets written
//! down somewhere worse than here.

use yew::prelude::*;

use crate::api;
use yewi::prelude::*;

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
    let confirming = use_state(|| false);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);

    // Completed here rather than by the agent, which behind a proxy does not
    // reliably know the address it is reached on. The browser is already talking
    // to the right one.
    let url = origin()
        .map(|origin| format!("{origin}{}", props.path))
        .unwrap_or_else(|| props.path.to_string());

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
            <Field label="Send deliveries to">
                <Cluster nowrap=true gap={Space::Sm}>
                    <TextInput class="webhook-address__url" value={url.clone()} readonly=true mono=true />
                    <CopyButton value={url.clone()}>{ "Copy" }</CopyButton>
                </Cluster>
            </Field>
            if *confirming {
                <Alert class="webhook-address__confirm" tone={Tone::Warn} actions={html! {
                    <>
                        <Button
                            variant={ButtonVariant::Danger}
                            size={Size::Small}
                            on_click={on_rotate}
                            loading={*busy}
                        >
                            { "Issue a new address" }
                        </Button>
                        <Button
                            variant={ButtonVariant::Subtle}
                            size={Size::Small}
                            on_click={on_cancel}
                            disabled={*busy}
                        >
                            { "Keep this one" }
                        </Button>
                    </>
                }}>
                    { "A new address takes effect at once, and this one stops working. \
                       Anything already sending here will need updating." }
                </Alert>
            } else {
                <Button
                    class="webhook-address__rotate"
                    variant={ButtonVariant::Subtle}
                    size={Size::Small}
                    icon={icons::REFRESH_CW}
                    on_click={on_ask}
                >
                    { "Issue a new address" }
                </Button>
            }
            if let Some(message) = (*error).clone() {
                <Text tag={TextTag::P} size={TextSize::Sm} tone={Tone::Danger}>{ message }</Text>
            }
        </div>
    }
}

fn origin() -> Option<String> {
    web_sys::window()?.location().origin().ok()
}
