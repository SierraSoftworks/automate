use htmd::{Element, element_handler::{HandlerResult, Handlers}};
use reqwest::Url;

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
        .add_handler(vec!["a"], move |handlers: &dyn Handlers, element: Element| {
            if let Some(href) = element.attrs.iter().find(|a| a.name.local.to_string() == "href") {
                let new_url = base_url.join(&href.value.to_string()).unwrap_or_else(|_| base_url.clone());
                let content = handlers.walk_children(element.node).content;

                Some(HandlerResult {
                    content: format!("[{}]({})", content, new_url),
                    markdown_translated: true,
                })
            } else {
                handlers.fallback(element)
            }
        })
        .build()
        .convert(html)
        .unwrap_or_default()
}