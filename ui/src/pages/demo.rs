//! The control gallery: every control the admin UI ships with, rendered in each
//! state it can be in.
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

use chrono::{Datelike, Utc};
use yew::prelude::*;

use crate::components::{Alert, AlertKind, PageTitle};
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
                kind={AlertKind::Info}
                title="The gallery needs demo mode"
                message="Some of these controls load their contents from the agent. Demo mode \
                         serves them from fixtures instead, so the gallery can run without one."
            >
                <a class="btn btn--primary" href="/demo/controls?demo">
                    { "Open in demo mode" }
                </a>
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
            <Alert
                kind={AlertKind::Warning}
                title="No such control"
                message={format!("The gallery has nothing called \"{}\".", requested.unwrap_or_default())}
            />
        });
    };

    let nav = html! {
        <nav class="gallery__nav" aria-label="Controls">
            { for CONTROLS.iter().map(|control| {
                let active = control.slug == selected.slug;
                html! {
                    // A full-page navigation, because a client-side one drops the
                    // query string and with it the demo mode this page needs.
                    <a
                        key={control.slug}
                        class={classes!("gallery__link", active.then_some("gallery__link--active"))}
                        href={util::nav_href(&format!("/demo/controls/{}", control.slug))}
                        aria-current={active.then_some("page")}
                    >
                        { control.name }
                    </a>
                }
            }) }
        </nav>
    };

    shell(html! {
        <div class="gallery">
            { nav }
            <div class="gallery__body">
                <header class="gallery__heading">
                    <h2 class="gallery__title">{ selected.name }</h2>
                    <p class="gallery__blurb">{ selected.blurb }</p>
                </header>
                { (selected.view)() }
            </div>
        </div>
    })
}

/// The page chrome shared by the gallery's states.
fn shell(body: Html) -> Html {
    html! {
        <div class="app-shell">
            <main class="app-main">
                <div class="app-container">
                    <PageTitle
                        title="Controls"
                        subtitle="Every control the admin UI ships with, in each state it can be in."
                    >
                        <a class="btn btn--small" href={util::nav_href("/admin")}>
                            { "Back to the admin UI" }
                        </a>
                    </PageTitle>
                    { body }
                </div>
            </main>
            <footer class="app-footer">
                <p>{ format!("Copyright © Sierra Softworks {}", Utc::now().year()) }</p>
            </footer>
        </div>
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
                <h3 class="specimen__label">{ props.label.clone() }</h3>
                if let Some(note) = &props.note {
                    <p class="specimen__note">{ note.clone() }</p>
                }
            </div>
            <div class="specimen__stage">{ props.children.clone() }</div>
        </section>
    }
}
