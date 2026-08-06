//! A button which reveals a short menu of commands or choices.
//!
//! Optionally a *split* button: one action carried out by clicking the button
//! itself, with the rest folded into the menu beside it. That shape exists for
//! rows of things where one action is what people came for and the others are
//! occasional — a row of equally-weighted buttons costs the width the row needs
//! to say what the actions are being done *to*, and makes the destructive one
//! exactly as easy to hit as the ordinary one.

use yew::prelude::*;

use super::form::ButtonKind;

#[derive(Clone, PartialEq)]
pub struct MenuButtonOption {
    pub value: AttrValue,
    pub label: AttrValue,
    pub section: Option<AttrValue>,

    /// Whether choosing this destroys something.
    pub destructive: bool,
}

impl MenuButtonOption {
    pub fn new(value: impl Into<AttrValue>, label: impl Into<AttrValue>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            section: None,
            destructive: false,
        }
    }

    pub fn in_section(mut self, section: impl Into<AttrValue>) -> Self {
        self.section = Some(section.into());
        self
    }

    /// Marks this as an action that destroys something, so it reads as one
    /// before it is chosen rather than after.
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }
}

#[derive(Properties, PartialEq)]
pub struct MenuButtonProps {
    pub label: AttrValue,
    pub options: Vec<MenuButtonOption>,
    pub onselect: Callback<String>,

    /// The action the button itself carries out, which turns this into a split
    /// button. Without one, clicking anywhere on the button opens the menu.
    #[prop_or_default]
    pub onclick: Option<Callback<MouseEvent>>,

    /// What the menu is a menu of, for anything reading the page aloud. Only
    /// used by a split button, where the chevron is a control of its own with no
    /// visible label.
    #[prop_or_default]
    pub menu_label: Option<AttrValue>,

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
                    { for props.options.iter().scan(None::<AttrValue>, |current_section, option| {
                        let section = option.section.clone().filter(|section| {
                            current_section.as_ref() != Some(section)
                        });
                        *current_section = option.section.clone();
                        let value = option.value.to_string();
                        let open = open.clone();
                        let onselect = props.onselect.clone();
                        let onclick = Callback::from(move |_: MouseEvent| {
                            open.set(false);
                            onselect.emit(value.clone());
                        });

                        Some(html! {
                            <li key={option.value.to_string()} role="none">
                                if let Some(section) = section {
                                    <span class="menu-button__section">{ section }</span>
                                }
                                <button
                                    class={classes!(
                                        "menu-button__item",
                                        option.destructive
                                            .then_some("menu-button__item--destructive"),
                                    )}
                                    role="menuitem"
                                    {onclick}
                                >
                                    { option.label.clone() }
                                </button>
                            </li>
                        })
                    }) }
                </ul>
            </>
        }
    } else {
        html! {}
    };

    let chevron = html! {
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
    };

    let button_class = classes!(
        "btn",
        props.kind.class(),
        props.small.then_some("btn--small"),
    );

    let has_options = !props.options.is_empty();

    let control = match &props.onclick {
        // A split button. The chevron is dropped entirely when the menu would be
        // empty, rather than left as a control that opens nothing — which leaves
        // an ordinary button, which is what the row then has.
        Some(onclick) => html! {
            <>
                <button
                    type="button"
                    class={button_class.clone()}
                    onclick={onclick.clone()}
                    disabled={props.disabled}
                    title={props.title.clone()}
                >
                    { for props.children.iter() }
                    { props.label.clone() }
                </button>

                if has_options {
                    <button
                        type="button"
                        class={classes!(button_class, "menu-button__toggle")}
                        onclick={toggle}
                        disabled={props.disabled}
                        aria-haspopup="menu"
                        aria-expanded={(*open).to_string()}
                        aria-label={
                            props.menu_label.clone().unwrap_or_else(|| "More actions".into())
                        }
                    >
                        { chevron }
                    </button>
                }
            </>
        },
        None => html! {
            <button
                type="button"
                class={button_class}
                onclick={toggle}
                disabled={props.disabled || !has_options}
                aria-haspopup="menu"
                aria-expanded={(*open).to_string()}
                title={props.title.clone()}
            >
                { for props.children.iter() }
                { props.label.clone() }
                { chevron }
            </button>
        },
    };

    html! {
        <div
            class={classes!(
                "menu-button",
                props.onclick.is_some().then_some("menu-button--split"),
            )}
            {onkeydown}
        >
            { control }
            { menu }
        </div>
    }
}
