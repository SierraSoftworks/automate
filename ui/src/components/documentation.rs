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

use yew::prelude::*;
use yewi::prelude::*;

#[derive(Properties, PartialEq)]
pub struct DocumentationProps {
    /// The guidance, in Markdown.
    pub markdown: AttrValue,
}

/// The setup guidance a workflow type ships with, folded away until asked for.
///
/// The text is rendered by yewi's `Markdown`, which escapes any raw HTML in the
/// source and opens links in a new tab so that following one does not discard a
/// half-filled form.
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

    html! {
        <section class="documentation">
            <Button
                variant={ButtonVariant::Subtle}
                size={Size::Small}
                icon={if *open { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT }}
                on_click={on_toggle}
                attrs={attrs! { "aria-expanded" => if *open { "true" } else { "false" } }}
            >
                if *open { { "Hide setup guidance" } } else { { "How does this work?" } }
            </Button>
            if *open {
                <div class="documentation__body">
                    <Markdown
                        source={props.markdown.clone()}
                        top_heading={HeadingLevel::H3}
                        external_links=true
                    />
                </div>
            }
        </section>
    }
}
