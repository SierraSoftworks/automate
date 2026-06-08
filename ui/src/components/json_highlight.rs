use serde_json::Value;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct JsonHighlightProps {
    /// The JSON value to render with syntax highlighting.
    pub value: Value,
}

/// Renders a [`serde_json::Value`] as lightweight, syntax-highlighted JSON
/// inside a `<pre><code>` block.
///
/// Every token's text is emitted through Yew's `{}` interpolation, which
/// HTML-escapes it, so untrusted payload data can never inject markup. This is a
/// deliberately lightweight strategy: it walks the already-parsed value rather
/// than pulling in a general-purpose highlighter, and falls back to rendering
/// the raw, unhighlighted text if the value cannot be serialised.
#[function_component(JsonHighlight)]
pub fn json_highlight(props: &JsonHighlightProps) -> Html {
    let body = match serde_json::to_string(&props.value) {
        Ok(_) => {
            let mut out = Vec::new();
            highlight_value(&props.value, 0, &mut out);
            html! { <>{ for out }</> }
        }
        Err(_) => html! { { props.value.to_string() } },
    };

    html! {
        <pre class="db-entity__payload"><code class="json">{ body }</code></pre>
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
