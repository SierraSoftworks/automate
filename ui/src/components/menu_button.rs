//! A button which reveals a short menu of commands or choices.

use yew::prelude::*;

use super::form::ButtonKind;

#[derive(Clone, PartialEq)]
pub struct MenuButtonOption {
    pub value: AttrValue,
    pub label: AttrValue,
}

impl MenuButtonOption {
    pub fn new(value: impl Into<AttrValue>, label: impl Into<AttrValue>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct MenuButtonProps {
    pub label: AttrValue,
    pub options: Vec<MenuButtonOption>,
    pub onselect: Callback<String>,

    #[prop_or_default]
    pub kind: ButtonKind,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub small: bool,

    #[prop_or_default]
    pub title: Option<AttrValue>,

    #[prop_or_default]
    pub children: Children,
}

#[function_component(MenuButton)]
pub fn menu_button(props: &MenuButtonProps) -> Html {
    let open = use_state(|| false);

    let toggle = {
        let open = open.clone();
        Callback::from(move |_: MouseEvent| open.set(!*open))
    };

    let onkeydown = {
        let open = open.clone();
        Callback::from(move |event: KeyboardEvent| {
            if event.key() == "Escape" {
                open.set(false);
            }
        })
    };

    let menu = if *open {
        let close = {
            let open = open.clone();
            Callback::from(move |_: MouseEvent| open.set(false))
        };

        html! {
            <>
                <div class="menu-button__backdrop" onclick={close} />
                <ul class="menu-button__list" role="menu">
                    { for props.options.iter().map(|option| {
                        let value = option.value.to_string();
                        let open = open.clone();
                        let onselect = props.onselect.clone();
                        let onclick = Callback::from(move |_: MouseEvent| {
                            open.set(false);
                            onselect.emit(value.clone());
                        });

                        html! {
                            <li key={option.value.to_string()} role="none">
                                <button class="menu-button__item" role="menuitem" {onclick}>
                                    { option.label.clone() }
                                </button>
                            </li>
                        }
                    }) }
                </ul>
            </>
        }
    } else {
        html! {}
    };

    html! {
        <div class="menu-button" {onkeydown}>
            <button
                type="button"
                class={classes!(
                    "btn",
                    props.kind.class(),
                    props.small.then_some("btn--small"),
                )}
                onclick={toggle}
                disabled={props.disabled || props.options.is_empty()}
                aria-haspopup="menu"
                aria-expanded={(*open).to_string()}
                title={props.title.clone()}
            >
                { for props.children.iter() }
                { props.label.clone() }
                <span
                    class={classes!(
                        "menu-button__chevron",
                        (*open).then_some("menu-button__chevron--open"),
                    )}
                    aria-hidden="true"
                >
                    <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor"
                        stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                        <polyline points="6 9 12 15 18 9" />
                    </svg>
                </span>
            </button>
            { menu }
        </div>
    }
}
