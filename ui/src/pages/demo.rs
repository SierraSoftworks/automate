//! The control gallery: every control Automate builds on top of yewi, rendered
//! in each state it can be in. (yewi's own controls are documented on its
//! reference site.)
//!
//! Reviewing a control normally means finding a page that happens to use it and
//! then contriving the data that puts it in the state you wanted to look at — an
//! alert only appears when something has failed, a connection badge only appears
//! when a connection is broken. Most of those states are cheap to *describe* and
//! expensive to *reach*, so they go unlooked-at, and a control regresses in the
//! one case nobody could be bothered to reproduce.
//!
//! This page reaches them directly. It renders each control beside the others of
//! its kind, so a change to shared styling is visible everywhere it lands rather
//! than only where it was noticed.
//!
//! It is a development tool: compiled only into debug builds, and served only
//! when demo mode is active, since the controls that fetch something have to be
//! talking to the fixtures rather than to nothing.

mod specimens;

use yew::prelude::*;

use yewi::prelude::*;

use crate::components::Footer;
use crate::fixtures;
use crate::util;

use specimens::CONTROLS;

#[derive(Properties, PartialEq, Default)]
pub struct DemoControlsProps {
    /// The control to show. Absent shows the first one in the gallery.
    #[prop_or_default]
    pub control: Option<AttrValue>,
}

#[function_component(DemoControls)]
pub fn demo_controls(props: &DemoControlsProps) -> Html {
    // Several specimens render controls that fetch (the connections panel, the
    // connect menu, the pickers in a dynamic form). Without demo mode those talk
    // to an agent that is not there, and the gallery would show a wall of
    // network errors instead of the controls it exists to show.
    if !fixtures::is_demo() {
        return shell(html! {
            <Alert
                tone={Tone::Info}
                title="The gallery needs demo mode"
                actions={html! {
                    <Button variant={ButtonVariant::Primary} size={Size::Small} href="/demo/controls?demo">
                        { "Open in demo mode" }
                    </Button>
                }}
            >
                { "Some of these controls load their contents from the agent. Demo mode \
                   serves them from fixtures instead, so the gallery can run without one." }
            </Alert>
        });
    }

    let requested = props.control.as_ref().map(|slug| slug.as_str());
    let selected = match requested {
        Some(slug) => CONTROLS.iter().find(|control| control.slug == slug),
        None => CONTROLS.first(),
    };

    let Some(selected) = selected else {
        return shell(html! {
            <Alert tone={Tone::Warn} title="No such control">
                { format!("The gallery has nothing called \"{}\".", requested.unwrap_or_default()) }
            </Alert>
        });
    };

    let nav = html! {
        <Sidebar label="Controls" class="gallery__nav">
            { for CONTROLS.iter().map(|control| html! {
                <SidebarItem
                    key={control.slug}
                    href={util::nav_href(&format!("/demo/controls/{}", control.slug))}
                    current={control.slug == selected.slug}
                >
                    { control.name }
                </SidebarItem>
            }) }
        </Sidebar>
    };

    shell(html! {
        <div class="gallery">
            { nav }
            <div class="gallery__body">
                <header class="gallery__heading">
                    <Heading level={HeadingLevel::H2}>{ selected.name }</Heading>
                    <Text tag={TextTag::P} color={TextColor::Muted}>{ selected.blurb }</Text>
                </header>
                { (selected.view)() }
            </div>
        </div>
    })
}

/// The page chrome shared by the gallery's states.
fn shell(body: Html) -> Html {
    let actions = html! {
        <Button size={Size::Small} icon={icons::ARROW_LEFT} href={util::nav_href("/admin")}>
            { "Back to the admin UI" }
        </Button>
    };

    html! {
        <AppShell class="app-shell app-shell--bare">
            <div class="app-container">
                <PageHeader
                    title="Controls"
                    subtitle="The controls Automate builds on top of yewi, in each state they can be in."
                    {actions}
                />
                { body }
            </div>
            <Footer />
        </AppShell>
    }
}

#[derive(Properties, PartialEq)]
pub struct SpecimenProps {
    /// What this specimen is demonstrating.
    pub label: AttrValue,

    /// Why it is worth looking at, when the label does not say.
    #[prop_or_default]
    pub note: Option<AttrValue>,

    #[prop_or_default]
    pub children: Html,
}

/// One labelled example of a control, on a plain background so the control's own
/// borders and shadows are the only ones on screen.
#[function_component(Specimen)]
pub fn specimen(props: &SpecimenProps) -> Html {
    html! {
        <section class="specimen">
            <div class="specimen__caption">
                <Heading level={HeadingLevel::H3} size={TextSize::Md}>{ props.label.clone() }</Heading>
                if let Some(note) = &props.note {
                    <Text tag={TextTag::P} size={TextSize::Sm} color={TextColor::Muted}>{ note.clone() }</Text>
                }
            </div>
            <div class="specimen__stage">{ props.children.clone() }</div>
        </section>
    }
}
