use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct CsrfTokenProps {
    /// The signed CSRF token to embed in the enclosing form.
    pub token: AttrValue,
}

/// Renders the hidden form field carrying the CSRF token expected by the admin
/// endpoints. Place this inside any state-changing `POST` form.
#[function_component(CsrfToken)]
pub fn csrf_token(props: &CsrfTokenProps) -> Html {
    html! {
        <input type="hidden" name="csrf_token" value={props.token.clone()} />
    }
}
