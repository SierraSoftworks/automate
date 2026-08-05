//! Rendering the guidance a workflow type ships with.
//!
//! Setting up a webhook workflow means knowing where in somebody else's admin
//! interface to paste an address, what a filter can match on, and what a switch
//! actually turns on. That is a paragraph with a link and a code sample in it,
//! not a sentence, so the agent ships it as Markdown and this draws it.
//!
//! # Why rendering HTML here is safe
//!
//! The Markdown is written in the agent's own source by whoever added the
//! workflow type. It never comes from a user, a payload, or a stored record, so
//! there is nothing here for anybody to inject. That is a property of where the
//! text comes from rather than of this component, so it is worth saying plainly:
//! if documentation ever becomes something a user can supply, this has to start
//! sanitising and this comment has to go.

use pulldown_cmark::{Options, Parser, html};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct DocumentationProps {
    /// The Markdown the workflow type ships with.
    pub markdown: AttrValue,
}

#[function_component(Documentation)]
pub fn documentation(props: &DocumentationProps) -> Html {
    let open = use_state(|| false);

    let on_toggle = {
        let open = open.clone();
        Callback::from(move |_| open.set(!*open))
    };

    if props.markdown.is_empty() {
        return html! {};
    }

    // Collapsed by default. Somebody adding their fourth RSS feed does not need
    // to scroll past an explanation of RSS to reach the form, and somebody
    // adding their first will not miss a control this size.
    html! {
        <section class="documentation">
            <button
                class="documentation__toggle"
                aria-expanded={if *open { "true" } else { "false" }}
                onclick={on_toggle}
            >
                if *open { { "Hide setup guidance" } } else { { "How does this work?" } }
            </button>

            if *open {
                <div class="documentation__body">
                    { render(&props.markdown) }
                </div>
            }
        </section>
    }
}

/// Turns the Markdown into nodes Yew can mount.
fn render(markdown: &str) -> Html {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let mut rendered = String::new();
    html::push_html(&mut rendered, Parser::new_ext(markdown, options));

    // Links go to somebody else's site, so they open in a new tab and carry the
    // usual disclaimers — leaving the form half-filled to read a provider's
    // documentation is exactly the thing this is meant to save.
    let rendered = rendered.replace(
        "<a href=",
        "<a target=\"_blank\" rel=\"noopener noreferrer\" href=",
    );

    Html::from_html_unchecked(AttrValue::from(rendered))
}
