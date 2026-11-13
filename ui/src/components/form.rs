//! The few form pieces this application adds on top of yewi's controls.
//!
//! Everything a user configures is entered through yewi's `Field`, `TextInput`,
//! `TextArea`, `NumberInput`, `SecretInput` and `ToggleSwitch`. What is left
//! here is the part that is specific to how Automate describes a choice: a
//! value that may no longer be on offer, and a choice that may be cleared.

use std::rc::Rc;

use yew::prelude::*;
use yewi::prelude::*;

/// One option of a [`ChoiceSelect`]: the value stored and the label shown.
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

fn default_placeholder() -> AttrValue {
    "Choose one".into()
}

#[derive(Properties, PartialEq)]
pub struct ChoiceSelectProps {
    /// The chosen value, or `None` when nothing is chosen.
    pub value: Option<AttrValue>,

    /// Fired with the new choice, or `None` when it is cleared.
    pub onchange: Callback<Option<String>>,

    pub options: Vec<SelectOption>,

    #[prop_or_else(default_placeholder)]
    pub placeholder: AttrValue,

    /// Whether "nothing" is a choice the user may return to. A required choice
    /// only shows the placeholder until something is picked.
    #[prop_or_default]
    pub clearable: bool,

    #[prop_or_default]
    pub disabled: bool,

    #[prop_or_default]
    pub invalid: bool,
}

/// A native select over a list the agent supplied.
///
/// Two things separate it from a bare `NativeSelect`. A stored value that is no
/// longer among the options is still shown, labelled as such, rather than being
/// silently replaced by whatever happens to be first — the user should see that
/// what they chose has gone. And an optional choice keeps an entry for "nothing",
/// so it can be cleared again.
#[function_component(ChoiceSelect)]
pub fn choice_select(props: &ChoiceSelectProps) -> Html {
    let mut options: Vec<(AttrValue, AttrValue)> = Vec::with_capacity(props.options.len() + 2);

    if props.clearable {
        options.push((AttrValue::from(""), props.placeholder.clone()));
    }

    if let Some(current) = &props.value
        && !props.options.iter().any(|option| &option.value == current)
    {
        options.push((
            current.clone(),
            format!("{current} (no longer available)").into(),
        ));
    }

    options.extend(
        props
            .options
            .iter()
            .map(|option| (option.value.clone(), option.label.clone())),
    );

    let on_change = props
        .onchange
        .reform(|value: String| (!value.is_empty()).then_some(value));

    html! {
        <NativeSelect
            options={Rc::<[_]>::from(options)}
            value={props.value.clone().unwrap_or_default()}
            {on_change}
            placeholder={(!props.clearable).then(|| props.placeholder.clone())}
            disabled={props.disabled}
            invalid={props.invalid}
        />
    }
}

/// The value of the control a focus event came from.
///
/// yewi's inputs report blur as the event itself; the forms here want the text
/// that was left behind, so that it can be trimmed once the user has finished
/// typing rather than while they are still in the middle of a word.
pub fn blur_value(event: &FocusEvent) -> Option<String> {
    let target = event.target()?;
    js_sys::Reflect::get(&target, &"value".into())
        .ok()?
        .as_string()
}
