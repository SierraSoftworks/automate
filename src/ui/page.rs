use chrono::{Datelike, Utc};
use yew::prelude::*;

const defaultStyles: &str = include_str!("styles.css");

#[derive(Properties, PartialEq)]
pub struct PageProps {
    pub title: Option<&'static str>,
    #[prop_or_default]
    pub children: Html,
}

#[function_component(Page)]
pub fn page(props: &PageProps) -> Html {
    html! {
        <html>
            <head>
                <title>{props.title.unwrap_or("Automate | Sierra Softworks")}</title>
                <meta
                    http-equiv="Content-Type"
                    content="text/html; charset=utf-8"
                />
                <meta
                    name="viewport"
                    content="width=device-width, initial-scale=1.0"
                />

                <meta name="author" content="Sierra Softworks" />
                <meta
                    name="description"
                    content="Automate is a simple, self-hosted automation platform to replace IFTTT."
                />

                <link
                    rel="icon"
                    href="https://cdn.sierrasoftworks.com/logos/logo.ico"
                    type="image/x-icon"
                />

                <style>{ defaultStyles }</style>
            </head>

            <body>
                <div class="header">
                    <img
                        src="https://cdn.sierrasoftworks.com/logos/logo.svg"
                        alt="The Sierra Softworks logo."
                    />
                </div>

                {props.children.clone()}

                <footer>
                    <p>
                        { format!("Copyright © Sierra Softworks {}", Utc::now().year()) }
                    </p>
                </footer>
            </body>
        </html>
    }
}