use gloo_timers::callback::Timeout;
use serde_json::Value;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct JsonHighlightProps {
    /// The JSON value to render with syntax highlighting.
    pub value: Value,
}

/// Renders a [`serde_json::Value`] as lightweight, syntax-highlighted JSON
/// inside a `<pre><code>` block, with a copy-to-clipboard control revealed on
/// hover.
///
/// Every token's text is emitted through Yew's `{}` interpolation, which
/// HTML-escapes it, so untrusted payload data can never inject markup. This is a
/// deliberately lightweight strategy: it walks the already-parsed value rather
/// than pulling in a general-purpose highlighter, and falls back to rendering
/// the raw, unhighlighted text if the value cannot be serialised.
#[function_component(JsonHighlight)]
pub fn json_highlight(props: &JsonHighlightProps) -> Html {
    // The pretty-printed text shared by both the highlighted view and the
    // clipboard copy. `to_string_pretty` uses the same two-space indentation as
    // the highlighter, so the copied text matches what is displayed.
    let pretty = serde_json::to_string_pretty(&props.value)
        .unwrap_or_else(|_| props.value.to_string());

    let body = match serde_json::to_string(&props.value) {
        Ok(_) => {
            let mut out = Vec::new();
            highlight_value(&props.value, 0, &mut out);
            html! { <>{ for out }</> }
        }
        Err(_) => html! { { props.value.to_string() } },
    };

    // Tracks whether the payload was just copied so the button can briefly
    // confirm with a checkmark before reverting to the copy glyph.
    let copied = use_state(|| false);
    let onclick = {
        let copied = copied.clone();
        Callback::from(move |_: MouseEvent| {
            copy_to_clipboard(pretty.clone());
            copied.set(true);
            let copied = copied.clone();
            // Revert the confirmation after a short delay.
            Timeout::new(1_500, move || copied.set(false)).forget();
        })
    };

    let (icon, label) = if *copied {
        (check_icon(), "Copied")
    } else {
        (copy_icon(), "Copy to clipboard")
    };
    let button_class = classes!(
        "json-highlight__copy",
        copied.then_some("json-highlight__copy--copied"),
    );

    html! {
        <div class="json-highlight">
            <button
                type="button"
                class={button_class}
                title={label}
                aria-label={label}
                {onclick}
            >
                { icon }
            </button>
            <pre class="db-entity__payload"><code class="json">{ body }</code></pre>
        </div>
    }
}

/// Writes `text` to the system clipboard via the async Clipboard API, ignoring
/// the outcome (the browser surfaces its own permission prompts on failure).
fn copy_to_clipboard(text: String) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let promise = window.navigator().clipboard().write_text(&text);
    spawn_local(async move {
        let _ = JsFuture::from(promise).await;
    });
}

/// A two-overlapping-sheets glyph indicating the copy-to-clipboard action.
fn copy_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
        </svg>
    }
}

/// A checkmark glyph confirming a successful copy.
fn check_icon() -> Html {
    html! {
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor"
            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <polyline points="20 6 9 17 4 12" />
        </svg>
    }
}

/// Returns the JSON-quoted, escaped form of a string (matching serde_json's own
/// escaping), e.g. `hello "world"` becomes `"hello \"world\""`.
fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("{text:?}"))
}

/// Recursively appends the highlighted spans for `value` to `out`, indented at
/// `depth` levels (two spaces each).
fn highlight_value(value: &Value, depth: usize, out: &mut Vec<Html>) {
    match value {
        Value::Null => out.push(html! { <span class="tok-null">{ "null" }</span> }),
        Value::Bool(b) => out.push(html! { <span class="tok-bool">{ b.to_string() }</span> }),
        Value::Number(n) => out.push(html! { <span class="tok-num">{ n.to_string() }</span> }),
        Value::String(s) => out.push(html! { <span class="tok-str">{ quote(s) }</span> }),
        Value::Array(items) => {
            if items.is_empty() {
                out.push(html! { <span class="tok-punct">{ "[]" }</span> });
                return;
            }
            out.push(html! { <span class="tok-punct">{ "[" }</span> });
            let inner = depth + 1;
            for (i, item) in items.iter().enumerate() {
                out.push(html! { { format!("\n{}", indent(inner)) } });
                highlight_value(item, inner, out);
                if i + 1 < items.len() {
                    out.push(html! { <span class="tok-punct">{ "," }</span> });
                }
            }
            out.push(html! { { format!("\n{}", indent(depth)) } });
            out.push(html! { <span class="tok-punct">{ "]" }</span> });
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push(html! { <span class="tok-punct">{ "{}" }</span> });
                return;
            }
            out.push(html! { <span class="tok-punct">{ "{" }</span> });
            let inner = depth + 1;
            for (i, (key, val)) in map.iter().enumerate() {
                out.push(html! { { format!("\n{}", indent(inner)) } });
                out.push(html! { <span class="tok-key">{ quote(key) }</span> });
                out.push(html! { <span class="tok-punct">{ ": " }</span> });
                highlight_value(val, inner, out);
                if i + 1 < map.len() {
                    out.push(html! { <span class="tok-punct">{ "," }</span> });
                }
            }
            out.push(html! { { format!("\n{}", indent(depth)) } });
            out.push(html! { <span class="tok-punct">{ "}" }</span> });
        }
    }
}

/// The indentation prefix for a given nesting `depth` (two spaces per level).
fn indent(depth: usize) -> String {
    "  ".repeat(depth)
}
