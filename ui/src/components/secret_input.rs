//! A shared secret, masked until its owner asks to see it.
//!
//! Masking a webhook token is worth doing — these get filled in over somebody's
//! shoulder, and a screen share is the usual way one leaks. Masking it
//! *permanently* is not: the whole job is to make the same string exist in two
//! places, and nobody can check that against the provider's settings page
//! without reading it back. So the mask is a default, not a wall.
//!
//! # Why the browser generates the value
//!
//! For the secrets that are merely agreed rather than issued, "a long random
//! string" is a placeholder that people satisfy with a short memorable one. The
//! generator turns that into a button. It runs here rather than in the agent
//! because a secret the agent minted would have to travel back over the wire to
//! be shown, and there is nothing the server knows about randomness that
//! `crypto.getRandomValues` does not.

use base64::prelude::*;
use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct SecretInputProps {
    pub id: AttrValue,
    pub value: AttrValue,
    pub onchange: Callback<String>,

    #[prop_or_default]
    pub onblur: Callback<String>,

    #[prop_or_default]
    pub placeholder: Option<AttrValue>,

    /// Offers a button that fills the field with a random value.
    #[prop_or_default]
    pub generator: bool,

    /// How many random bytes that button draws, before encoding.
    #[prop_or(32)]
    pub generator_bytes: usize,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,
}

#[function_component(SecretInput)]
pub fn secret_input(props: &SecretInputProps) -> Html {
    let revealed = use_state(|| false);

    let oninput = {
        let onchange = props.onchange.clone();
        Callback::from(move |event: InputEvent| {
            if let Some(input) = event.target_dyn_into::<HtmlInputElement>() {
                onchange.emit(input.value());
            }
        })
    };

    let onblur = {
        let blur = props.onblur.clone();
        Callback::from(move |event: FocusEvent| {
            if let Some(input) = event.target_dyn_into::<HtmlInputElement>() {
                blur.emit(input.value());
            }
        })
    };

    let on_reveal = {
        let revealed = revealed.clone();
        Callback::from(move |_: MouseEvent| revealed.set(!*revealed))
    };

    let on_generate = {
        let (onchange, revealed, bytes) = (
            props.onchange.clone(),
            revealed.clone(),
            props.generator_bytes,
        );

        Callback::from(move |_: MouseEvent| {
            // Nothing is emitted when the platform cannot supply randomness,
            // because the alternative to a generated secret is one the user
            // chooses, not a predictable one we filled in for them.
            if let Some(secret) = random_secret(bytes) {
                revealed.set(true);
                onchange.emit(secret);
            }
        })
    };

    // Nothing to unmask while the field is empty, so the button that would do it
    // is not drawn — but the room it takes is reserved either way, so the
    // placeholder does not reflow the moment somebody types.
    let unmaskable = !props.value.is_empty();
    let actions = usize::from(props.generator) + 1;

    html! {
        <div class="secret-input">
            <input
                id={props.id.clone()}
                class={classes!(
                    "field__input",
                    "secret-input__input",
                    format!("secret-input__input--actions-{actions}"),
                    props.invalid.then_some("field__input--invalid"),
                )}
                type={if *revealed { "text" } else { "password" }}
                value={props.value.clone()}
                placeholder={props.placeholder.clone()}
                disabled={props.disabled}
                autocomplete="off"
                spellcheck="false"
                {oninput}
                {onblur}
            />

            <div class="secret-input__actions">
                if unmaskable {
                    <button
                        type="button"
                        class="secret-input__action"
                        onclick={on_reveal}
                        disabled={props.disabled}
                        title={if *revealed { "Hide" } else { "Show" }}
                        aria-label={if *revealed { "Hide the secret" } else { "Show the secret" }}
                        aria-pressed={revealed.to_string()}
                    >
                        if *revealed { { eye_off_icon() } } else { { eye_icon() } }
                    </button>
                }

                if props.generator {
                    <button
                        type="button"
                        class="secret-input__action"
                        onclick={on_generate}
                        disabled={props.disabled}
                        title="Generate a new secret"
                        aria-label="Generate a new secret"
                    >
                        { refresh_icon() }
                    </button>
                }
            </div>
        </div>
    }
}

/// A URL-safe random string carrying `bytes` bytes of entropy.
///
/// URL-safe and unpadded because these end up in headers, query strings and
/// configuration files at the other end, and a `+`, `/` or `=` is the kind of
/// character that survives one of those and not the next.
fn random_secret(bytes: usize) -> Option<String> {
    let mut buffer = vec![0u8; bytes];
    web_sys::window()?
        .crypto()
        .ok()?
        .get_random_values_with_u8_array(&mut buffer)
        .ok()?;

    Some(BASE64_URL_SAFE_NO_PAD.encode(&buffer))
}

fn eye_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
            <circle cx="12" cy="12" r="3" />
        </svg>
    }
}

fn eye_off_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94" />
            <path d="M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19" />
            <path d="M14.12 14.12a3 3 0 1 1-4.24-4.24" />
            <line x1="1" y1="1" x2="23" y2="23" />
        </svg>
    }
}

fn refresh_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polyline points="23 4 23 10 17 10" />
            <polyline points="1 20 1 14 7 14" />
            <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
        </svg>
    }
}
