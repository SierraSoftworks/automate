//! Form controls.
//!
//! Everything a user configures — a connection, a workflow's source and target —
//! is entered through these, so they are built once here rather than assembled
//! per page. They deliberately share one shape: each takes its current `value`
//! and an `onchange` carrying the new one, leaving state with the caller.
//!
//! # Why the controls are unstyled by their own choosing
//!
//! Each renders the same `field__*` classes and defers to the stylesheet's
//! existing Element Plus derived tokens, so a control dropped into a new page
//! looks like the rest of the application without the page saying anything about
//! it.
//!
//! # Validation
//!
//! Controls display an error; they never decide one. What counts as valid
//! depends on the workflow type being edited and is settled by the agent, which
//! is the only thing that can be authoritative about it — so a control that
//! decided for itself would either duplicate that logic or contradict it.

// The kit is written as a whole rather than grown one page at a time, so that
// the pages built on it are consistent by construction instead of each inventing
// the control it happens to need first.
#![allow(dead_code)]

use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

/// A labelled row wrapping a single control.
///
/// Owns the label, the required marker, the help text and the error, so that
/// every field in the application lays out identically and a control only has to
/// render its own input.
#[derive(Properties, PartialEq)]
pub struct FieldProps {
    pub label: AttrValue,

    /// Associates the label with the control it describes.
    pub id: AttrValue,

    /// Marks the field as one the user must fill in.
    #[prop_or_default]
    pub required: bool,

    /// Guidance shown beneath the control. Hidden while an error is displayed,
    /// because the error is the more urgent of the two and stacking both pushes
    /// the next field down as the user types.
    #[prop_or_default]
    pub help: Option<AttrValue>,

    /// A problem with the current value, as reported by the agent.
    #[prop_or_default]
    pub error: Option<AttrValue>,

    pub children: Children,
}

#[function_component(Field)]
pub fn field(props: &FieldProps) -> Html {
    let mut class = classes!("field");
    if props.error.is_some() {
        class.push("field--invalid");
    }

    html! {
        <div class={class}>
            <label class="field__label" for={props.id.clone()}>
                { &props.label }
                if props.required {
                    <span class="field__required" aria-hidden="true">{ "*" }</span>
                }
            </label>

            <div class="field__control">
                { props.children.clone() }
            </div>

            if let Some(error) = &props.error {
                <p class="field__error" role="alert">{ error }</p>
            } else if let Some(help) = &props.help {
                <p class="field__help">{ help }</p>
            }
        </div>
    }
}

/// Reads the value out of an input event's target.
fn input_value(event: &InputEvent) -> Option<String> {
    event
        .target_dyn_into::<HtmlInputElement>()
        .map(|input| input.value())
        .or_else(|| {
            event
                .target_dyn_into::<HtmlTextAreaElement>()
                .map(|area| area.value())
        })
}

/// Reads the value out of a blur event's target.
fn blurred_value(event: &FocusEvent) -> Option<String> {
    event
        .target_dyn_into::<HtmlInputElement>()
        .map(|input| input.value())
        .or_else(|| {
            event
                .target_dyn_into::<HtmlTextAreaElement>()
                .map(|area| area.value())
        })
}

#[derive(Properties, PartialEq)]
pub struct TextInputProps {
    pub id: AttrValue,
    pub value: AttrValue,
    pub onchange: Callback<String>,

    #[prop_or_default]
    pub onblur: Callback<String>,

    #[prop_or_default]
    pub placeholder: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,

    /// Renders the value as dots and stops the browser offering to remember it.
    ///
    /// Used for the tokens a user pastes in. Once stored those are never sent
    /// back, so this only protects what is on screen while it is being typed.
    #[prop_or_default]
    pub secret: bool,
}

#[function_component(TextInput)]
pub fn text_input(props: &TextInputProps) -> Html {
    let onchange = props.onchange.clone();
    let oninput = Callback::from(move |event: InputEvent| {
        if let Some(value) = input_value(&event) {
            onchange.emit(value);
        }
    });
    let blur = props.onblur.clone();
    let onblur = Callback::from(move |event: FocusEvent| {
        if let Some(value) = blurred_value(&event) {
            blur.emit(value);
        }
    });

    html! {
        <input
            id={props.id.clone()}
            class={classes!("field__input", props.invalid.then_some("field__input--invalid"))}
            type={if props.secret { "password" } else { "text" }}
            value={props.value.clone()}
            placeholder={props.placeholder.clone()}
            disabled={props.disabled}
            autocomplete={if props.secret { "off" } else { "on" }}
            spellcheck={if props.secret { "false" } else { "true" }}
            {oninput}
            {onblur}
        />
    }
}

#[derive(Properties, PartialEq)]
pub struct TextAreaProps {
    pub id: AttrValue,
    pub value: AttrValue,
    pub onchange: Callback<String>,

    #[prop_or_default]
    pub onblur: Callback<String>,

    #[prop_or_default]
    pub placeholder: Option<AttrValue>,

    #[prop_or(3)]
    pub rows: u32,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,

    /// Renders in a monospaced face, for values whose alignment carries meaning
    /// such as a filter expression.
    #[prop_or_default]
    pub monospace: bool,
}

#[function_component(TextArea)]
pub fn text_area(props: &TextAreaProps) -> Html {
    let onchange = props.onchange.clone();
    let oninput = Callback::from(move |event: InputEvent| {
        if let Some(value) = input_value(&event) {
            onchange.emit(value);
        }
    });
    let blur = props.onblur.clone();
    let onblur = Callback::from(move |event: FocusEvent| {
        if let Some(value) = blurred_value(&event) {
            blur.emit(value);
        }
    });

    html! {
        <textarea
            id={props.id.clone()}
            class={classes!(
                "field__input",
                "field__textarea",
                props.monospace.then_some("field__textarea--mono"),
                props.invalid.then_some("field__input--invalid"),
            )}
            rows={props.rows.to_string()}
            value={props.value.clone()}
            placeholder={props.placeholder.clone()}
            disabled={props.disabled}
            {oninput}
            {onblur}
        />
    }
}

#[derive(Properties, PartialEq)]
pub struct NumberInputProps {
    pub id: AttrValue,

    /// Absent means the field is empty, which is distinct from zero.
    pub value: Option<i64>,
    pub onchange: Callback<Option<i64>>,

    #[prop_or_default]
    pub min: Option<i64>,

    #[prop_or_default]
    pub max: Option<i64>,

    #[prop_or_default]
    pub placeholder: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,
}

#[function_component(NumberInput)]
pub fn number_input(props: &NumberInputProps) -> Html {
    let onchange = props.onchange.clone();
    let oninput = Callback::from(move |event: InputEvent| {
        let Some(raw) = input_value(&event) else {
            return;
        };

        // An empty box means "unset" rather than zero, and a partially typed
        // value such as "-" is left alone so the field does not fight the user
        // mid-keystroke.
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            onchange.emit(None);
        } else if let Ok(parsed) = trimmed.parse::<i64>() {
            onchange.emit(Some(parsed));
        }
    });

    html! {
        <input
            id={props.id.clone()}
            class={classes!("field__input", props.invalid.then_some("field__input--invalid"))}
            type="number"
            value={props.value.map(|v| v.to_string()).unwrap_or_default()}
            min={props.min.map(|v| v.to_string())}
            max={props.max.map(|v| v.to_string())}
            placeholder={props.placeholder.clone()}
            disabled={props.disabled}
            {oninput}
        />
    }
}

/// One choice in a [`Select`].
#[derive(Clone, PartialEq)]
pub struct SelectOption {
    pub value: AttrValue,
    pub label: AttrValue,
}

impl SelectOption {
    pub fn new(value: impl Into<AttrValue>, label: impl Into<AttrValue>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct SelectProps {
    pub id: AttrValue,

    /// Absent means nothing is chosen, which shows the placeholder.
    pub value: Option<AttrValue>,
    pub onchange: Callback<Option<String>>,

    pub options: Vec<SelectOption>,

    /// Shown as a disabled first entry while nothing is chosen.
    #[prop_or(AttrValue::from("Choose one"))]
    pub placeholder: AttrValue,

    /// Lets the user return to having nothing chosen.
    #[prop_or_default]
    pub clearable: bool,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,
}

/// A single-choice picker.
///
/// Built on the browser's own `<select>`. A bespoke dropdown would allow richer
/// entries, but this is keyboard-navigable, usable on a touch device, and
/// accessible without any work on our part — none of which is worth trading away
/// until a picker genuinely needs to render something the browser cannot.
#[function_component(Select)]
pub fn select(props: &SelectProps) -> Html {
    let onchange = props.onchange.clone();
    let onchange = Callback::from(move |event: Event| {
        let Some(element) = event.target_dyn_into::<HtmlSelectElement>() else {
            return;
        };

        let value = element.value();
        onchange.emit(if value.is_empty() { None } else { Some(value) });
    });

    // The current value may name something no longer offered — a project that
    // has been renamed in Todoist, say. Showing it keeps the field honest about
    // what is stored instead of silently appearing to be set to the first entry.
    let missing = props.value.as_ref().filter(|current| {
        !props
            .options
            .iter()
            .any(|option| &&option.value == current)
    });

    html! {
        <select
            id={props.id.clone()}
            class={classes!("field__input", "field__select", props.invalid.then_some("field__input--invalid"))}
            value={props.value.clone()}
            disabled={props.disabled}
            {onchange}
        >
            <option value="" disabled={!props.clearable} selected={props.value.is_none()}>
                { &props.placeholder }
            </option>

            if let Some(missing) = missing {
                <option value={missing.clone()} selected=true>
                    { format!("{missing} (no longer available)") }
                </option>
            }

            { for props.options.iter().map(|option| html! {
                <option
                    key={option.value.as_str()}
                    value={option.value.clone()}
                    selected={props.value.as_ref() == Some(&option.value)}
                >
                    { &option.label }
                </option>
            }) }
        </select>
    }
}

#[derive(Properties, PartialEq)]
pub struct SwitchProps {
    pub id: AttrValue,
    pub checked: bool,
    pub onchange: Callback<bool>,

    /// Sits beside the switch, since a bare toggle says nothing about what it
    /// controls.
    #[prop_or_default]
    pub label: Option<AttrValue>,

    #[prop_or_default]
    pub disabled: bool,
}

#[function_component(Switch)]
pub fn switch(props: &SwitchProps) -> Html {
    let onchange = props.onchange.clone();
    let onclick = Callback::from(move |event: MouseEvent| {
        if let Some(input) = event.target_dyn_into::<HtmlInputElement>() {
            onchange.emit(input.checked());
        }
    });

    html! {
        <label class={classes!("switch", props.disabled.then_some("switch--disabled"))}>
            <input
                id={props.id.clone()}
                class="switch__input"
                type="checkbox"
                checked={props.checked}
                disabled={props.disabled}
                {onclick}
            />
            <span class="switch__track" aria-hidden="true"><span class="switch__thumb" /></span>
            if let Some(label) = &props.label {
                <span class="switch__label">{ label }</span>
            }
        </label>
    }
}

/// How prominent a [`Button`] should be.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonKind {
    /// The ordinary action on a page.
    #[default]
    Default,

    /// The one action a page exists for. At most one per view, or none is
    /// primary.
    Primary,

    /// An action that destroys something.
    Danger,

    /// An action that should be available without drawing the eye.
    Subtle,
}

impl ButtonKind {
    pub(crate) fn class(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Primary => Some("btn--primary"),
            Self::Danger => Some("btn--danger"),
            Self::Subtle => Some("btn--subtle"),
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct ButtonProps {
    pub onclick: Callback<MouseEvent>,

    #[prop_or_default]
    pub kind: ButtonKind,

    #[prop_or_default]
    pub disabled: bool,

    /// Disables the button and shows that something is happening, so a slow
    /// request cannot be submitted twice.
    #[prop_or_default]
    pub busy: bool,

    #[prop_or_default]
    pub small: bool,

    /// Used when the label alone does not describe the action, and for
    /// icon-only buttons.
    #[prop_or_default]
    pub title: Option<AttrValue>,

    #[prop_or_default]
    pub children: Children,
}

#[function_component(Button)]
pub fn button(props: &ButtonProps) -> Html {
    html! {
        <button
            type="button"
            class={classes!(
                "btn",
                props.kind.class(),
                props.small.then_some("btn--small"),
                props.busy.then_some("btn--busy"),
            )}
            onclick={props.onclick.clone()}
            disabled={props.disabled || props.busy}
            title={props.title.clone()}
            aria-busy={props.busy.then_some("true")}
        >
            { props.children.clone() }
        </button>
    }
}
