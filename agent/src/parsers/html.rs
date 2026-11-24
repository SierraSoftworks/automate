use htmd::{
    Element,
    element_handler::{HandlerResult, Handlers},
};
use reqwest::Url;
use tracing_batteries::prelude::*;

#[instrument("parsers.html.html_to_markdown", skip(html, base_url))]
pub fn html_to_markdown(html: &str, base_url: Url) -> String {
    htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style"])
        .options(htmd::options::Options {
            heading_style: htmd::options::HeadingStyle::Atx,
            bullet_list_marker: htmd::options::BulletListMarker::Asterisk,
            code_block_style: htmd::options::CodeBlockStyle::Fenced,
            code_block_fence: htmd::options::CodeBlockFence::Backticks,
            ..Default::default()
        })
        .add_handler(
            vec!["a"],
            move |handlers: &dyn Handlers, element: &Element| {
                // htmd 0.6 no longer exposes `Element::attrs`; attributes are
                // read through the element's DOM node instead.
                let href = element.node.attrs().get("href").map(str::to_owned);
                if let Some(href) = href {
                    let new_url = base_url.join(&href).unwrap_or_else(|_| base_url.clone());
                    let content = handlers.walk_children_content(element.node, element.context);

                    Some(HandlerResult {
                        content: format!("[{}]({})", content, new_url),
                        markdown_translated: true,
                    })
                } else {
                    handlers.fallback(element)
                }
            },
        )
        .build()
        .convert(html)
        .unwrap_or_default()
}
