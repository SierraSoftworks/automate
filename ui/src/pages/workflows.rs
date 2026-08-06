//! Creating and managing workflows.
//!
//! The forms on this page are not written here. The agent describes what each
//! workflow type needs collected and [`DynamicForm`] draws it, so a workflow
//! type added to the agent appears here without this file changing.
//!
//! Adding and editing share one form. They ask for the same things, and the one
//! difference — that an existing workflow's type is settled and cannot be
//! changed — belongs to the thing wrapping the form rather than to the form.

use std::rc::Rc;

use automate_api::{
    ConnectionSummary, FieldKind, RunOutcome, RunState, Workflow, WorkflowHealth, WorkflowTrigger,
    WorkflowTypeDescriptor,
};
use gloo_timers::callback::Timeout;
use yew::prelude::*;

use crate::api;
use crate::components::dynamic_form::{set_at, value_at};
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Documentation, DynamicForm, FetchedOptions, Field,
    JsonHighlight, MenuButton, MenuButtonOption, PageActions, StatusPill, StatusTone, Switch,
    TextInput, WebhookAddress,
};
use crate::search::{MatchContext, SearchContext};
use crate::util::{format_iso8601, short_relative};

/// What a form hands back when it is submitted.
#[derive(Clone, PartialEq)]
pub struct WorkflowValues {
    pub config: serde_json::Value,
    pub schedule: Option<String>,
    pub enabled: bool,
}

/// Says that an action was accepted, then takes it back down.
///
/// Running and resetting both finish somewhere the row cannot see, so without a
/// line like this the page answers the request by looking exactly as it did
/// before.
fn announce(notice: &UseStateHandle<Option<String>>, message: String) {
    notice.set(Some(message));

    let notice = notice.clone();
    Timeout::new(4_000, move || notice.set(None)).forget();
}

/// How a workflow's health reads on its row.
///
/// Only says "working" once there has been a run to say it about. A workflow
/// created a minute ago is not working and not failing, and claiming either
/// would be a guess dressed up as a status.
fn health_pill(health: &WorkflowHealth) -> Html {
    let failing = health.consecutive_failures > 0;

    let title = match &health.message {
        Some(message) if failing => message.clone(),
        _ if failing => format!("Its last run failed {}.", short_relative(health.at)),
        _ => format!("Its last run worked, {}.", short_relative(health.at)),
    };

    let label = match health.consecutive_failures {
        0 => "Working".to_string(),
        1 => "Failing".to_string(),
        runs => format!("Failing ({runs} runs)"),
    };

    html! {
        <StatusPill
            tone={if failing { StatusTone::Error } else { StatusTone::Ok }}
            {label}
            {title}
        />
    }
}

/// Whether a click landed on something that already does something of its own.
///
/// The row is clickable as a whole, and the switch and the action button sitting
/// in it are not obstacles somebody should have to aim around: a click on either
/// must do that thing and nothing else.
fn from_a_control(event: &MouseEvent) -> bool {
    let mut node = event.target_dyn_into::<web_sys::Element>();

    while let Some(element) = node {
        match element.tag_name().to_ascii_lowercase().as_str() {
            "a" | "button" | "input" | "label" | "select" | "summary" | "textarea" => return true,
            // The row itself, so the walk stops rather than climbing the page.
            "li" => return false,
            _ => {}
        }

        node = element.parent_element();
    }

    false
}

#[function_component(Workflows)]
pub fn workflows() -> Html {
    let workflows = use_state(Vec::<Workflow>::new);
    let types = use_state(Vec::<WorkflowTypeDescriptor>::new);
    let connections = use_state(Vec::<ConnectionSummary>::new);
    let error = use_state(|| None::<String>);
    let loading = use_state(|| true);
    let reload = use_state(|| 0u32);
    let chosen = use_state(|| None::<String>);

    {
        let (workflows, types, connections, error, loading) = (
            workflows.clone(),
            types.clone(),
            connections.clone(),
            error.clone(),
            loading.clone(),
        );

        use_effect_with(*reload, move |reload| {
            // Only the first pass shows the placeholder. A refresh after an edit
            // already has a list to show, and swapping it for "Loading…" throws
            // away every row's own state — including the line saying what just
            // happened to it.
            let first_load = *reload == 0;

            wasm_bindgen_futures::spawn_local(async move {
                if first_load {
                    loading.set(true);
                }

                match api::list_workflows().await {
                    Ok(found) => {
                        workflows.set(found);
                        error.set(None);
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }

                // The types and the linked accounts are what a form is drawn
                // from, so failing to load either shows up as the form being
                // unavailable rather than as an error over the list.
                if let Ok(found) = api::list_workflow_types().await {
                    types.set(found);
                }
                if let Ok(found) = api::list_service_connections().await {
                    connections.set(found);
                }

                loading.set(false);
            });
            || ()
        });
    }

    let on_changed = {
        let reload = reload.clone();
        Callback::from(move |_| reload.set(*reload + 1))
    };

    let on_type = {
        let chosen = chosen.clone();
        Callback::from(move |type_id: String| chosen.set(Some(type_id)))
    };

    let on_cancel_add = {
        let chosen = chosen.clone();
        Callback::from(move |_| chosen.set(None))
    };

    let on_created = {
        let (chosen, reload) = (chosen.clone(), reload.clone());
        Callback::from(move |_| {
            chosen.set(None);
            reload.set(*reload + 1);
        })
    };

    let type_options: Vec<MenuButtonOption> = types
        .iter()
        .map(|descriptor| MenuButtonOption::new(descriptor.id.clone(), descriptor.name.clone()))
        .collect();

    let chosen_descriptor = chosen
        .as_ref()
        .and_then(|id| types.iter().find(|descriptor| &descriptor.id == id))
        .cloned();

    let page_actions = use_context::<PageActions>();
    {
        let page_actions = page_actions.clone();
        let type_options = type_options.clone();
        let on_type = on_type.clone();
        let disabled = *loading || chosen_descriptor.is_some();
        use_effect_with((type_options.clone(), disabled), move |_| {
            if let Some(actions) = &page_actions {
                actions.set(html! {
                    <div class="page-title__actions">
                        <MenuButton
                            label="Add Workflow"
                            options={type_options}
                            onselect={on_type}
                            kind={ButtonKind::Primary}
                            {disabled}
                        />
                    </div>
                });
            }
            move || {
                if let Some(actions) = page_actions {
                    actions.clear();
                }
            }
        });
    }

    let search = use_context::<SearchContext>();
    let filter = search
        .as_ref()
        .map(|search| search.filter.clone())
        .unwrap_or_default();
    let visible_workflows: Vec<&Workflow> = workflows
        .iter()
        .filter(|workflow| {
            let text = format!("{} {} {}", workflow.name, workflow.type_id, workflow.config)
                .to_lowercase();
            filter.matches(&MatchContext {
                fields: &[("name", &workflow.name), ("kind", &workflow.type_id)],
                text: &text,
            })
        })
        .collect();

    let body = if *loading {
        html! { <p class="workflows__empty">{ "Loading…" }</p> }
    } else if workflows.is_empty() {
        html! {
            <p class="workflows__empty">
                { "You have no workflows yet. Add one to have Automate watch something for you." }
            </p>
        }
    } else if visible_workflows.is_empty() {
        html! { <p class="workflows__empty">{ "No workflows match your search." }</p> }
    } else {
        html! {
            <ul class="workflows__list">
                { for visible_workflows.into_iter().map(|workflow| html! {
                    <WorkflowRow
                        key={workflow.id.to_string()}
                        workflow={workflow.clone()}
                        descriptor={types.iter().find(|t| t.id == workflow.type_id).cloned()}
                        connections={(*connections).clone()}
                        on_changed={on_changed.clone()}
                    />
                }) }
            </ul>
        }
    };

    html! {
        <section class="workflows">
            if let Some(message) = (*error).clone() {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not load your workflows."
                    message={message}
                />
            }

            if let Some(descriptor) = chosen_descriptor {
                <AddWorkflow
                    {descriptor}
                    connections={(*connections).clone()}
                    on_created={on_created}
                    oncancel={on_cancel_add}
                />
            }

            { body }
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct WorkflowRowProps {
    workflow: Workflow,
    /// Absent when the agent no longer offers this workflow's type, which is
    /// what an upgrade that removed one looks like from here.
    descriptor: Option<WorkflowTypeDescriptor>,
    connections: Vec<ConnectionSummary>,
    on_changed: Callback<()>,
}

#[function_component(WorkflowRow)]
fn workflow_row(props: &WorkflowRowProps) -> Html {
    let editing = use_state(|| false);
    let busy = use_state(|| false);
    // The heading as well as the message, because a row can fail at two
    // different things and "we could not save this change" is untrue of one.
    let error = use_state(|| None::<(&'static str, String)>);
    // Set briefly after an action whose effect happens elsewhere. A run is
    // queued and finishes later, and a reset changes nothing the row shows, so
    // without this the page would answer both requests by looking unchanged.
    let notice = use_state(|| None::<String>);
    // Resetting throws away what a workflow remembers, and the consequence — a
    // backlog re-filed as though it were new — lands in somebody's task list
    // rather than here, so it is worth saying out loud before it happens.
    let confirming_reset = use_state(|| false);
    // What the row folds away: the address it receives deliveries on, and how
    // its last runs went. The runs are fetched only once this is open, since
    // they carry the payload each run was handed and nobody wants every row's
    // at once.
    let expanded = use_state(|| false);
    let workflow = &props.workflow;

    /// Saves a change to this workflow, whatever prompted it.
    fn save(
        id: String,
        values: WorkflowValues,
        busy: UseStateHandle<bool>,
        error: UseStateHandle<Option<(&'static str, String)>>,
        done: Callback<()>,
    ) {
        wasm_bindgen_futures::spawn_local(async move {
            busy.set(true);
            error.set(None);

            match api::update_workflow(
                &id,
                &values.config,
                values.schedule.as_deref(),
                values.enabled,
            )
            .await
            {
                Ok(_) => done.emit(()),
                Err(err) => error.set(Some(("We could not save this change.", err.to_string()))),
            }

            busy.set(false);
        });
    }

    // Pausing is an edit like any other, so it goes through the same call with
    // the configuration left as it is. Sending the whole thing is what the
    // endpoint expects; sending only the flag would read as clearing the rest.
    let on_toggle = {
        let (id, config, schedule, busy, error, on_changed) = (
            workflow.id.to_string(),
            workflow.config.clone(),
            workflow.schedule.clone(),
            busy.clone(),
            error.clone(),
            props.on_changed.clone(),
        );

        Callback::from(move |enabled: bool| {
            save(
                id.clone(),
                WorkflowValues {
                    config: config.clone(),
                    schedule: schedule.clone(),
                    enabled,
                },
                busy.clone(),
                error.clone(),
                on_changed.clone(),
            );
        })
    };

    let on_save = {
        let (id, busy, error, editing, on_changed) = (
            workflow.id.to_string(),
            busy.clone(),
            error.clone(),
            editing.clone(),
            props.on_changed.clone(),
        );

        Callback::from(move |values: WorkflowValues| {
            let editing = editing.clone();
            let on_changed = on_changed.clone();

            save(
                id.clone(),
                values,
                busy.clone(),
                error.clone(),
                Callback::from(move |_| {
                    editing.set(false);
                    on_changed.emit(());
                }),
            );
        })
    };

    let on_delete = {
        let (id, busy, on_changed) = (
            workflow.id.to_string(),
            busy.clone(),
            props.on_changed.clone(),
        );

        Callback::from(move |_| {
            let (id, busy, on_changed) = (id.clone(), busy.clone(), on_changed.clone());
            wasm_bindgen_futures::spawn_local(async move {
                busy.set(true);
                let _ = api::delete_workflow(&id).await;
                busy.set(false);
                on_changed.emit(());
            });
        })
    };

    let on_edit = {
        let editing = editing.clone();
        Callback::from(move |_| editing.set(!*editing))
    };

    // Runs the workflow now. Its schedule is left where it was, so this is an
    // extra run rather than one brought forward.
    let on_trigger = {
        let (id, busy, error, notice, on_changed) = (
            workflow.id.to_string(),
            busy.clone(),
            error.clone(),
            notice.clone(),
            props.on_changed.clone(),
        );

        Callback::from(move |_| {
            let (id, busy, error, notice, on_changed) = (
                id.clone(),
                busy.clone(),
                error.clone(),
                notice.clone(),
                on_changed.clone(),
            );

            wasm_bindgen_futures::spawn_local(async move {
                busy.set(true);
                error.set(None);

                match api::trigger_workflow(&id).await {
                    Ok(()) => {
                        announce(&notice, "Queued to run now.".to_string());
                        on_changed.emit(());
                    }
                    Err(err) => {
                        error.set(Some(("We could not run this workflow.", err.to_string())))
                    }
                }

                busy.set(false);
            });
        })
    };

    // Forgets the watermarks and snapshots this workflow keeps between runs, so
    // that the next run starts from nothing.
    //
    // Deliberately does not reload the list. Nothing a row shows is derived from
    // the state that was cleared, and reloading replaces the list with its
    // loading placeholder — which would take the row, and the line saying what
    // just happened, down with it.
    let on_reset = {
        let (id, busy, error, notice, confirming_reset) = (
            workflow.id.to_string(),
            busy.clone(),
            error.clone(),
            notice.clone(),
            confirming_reset.clone(),
        );

        Callback::from(move |_| {
            let (id, busy, error, notice, confirming_reset) = (
                id.clone(),
                busy.clone(),
                error.clone(),
                notice.clone(),
                confirming_reset.clone(),
            );

            wasm_bindgen_futures::spawn_local(async move {
                busy.set(true);
                error.set(None);

                match api::reset_workflow(&id).await {
                    Ok(cleared) => {
                        confirming_reset.set(false);
                        announce(
                            &notice,
                            match cleared {
                                1 => "Forgot 1 remembered value.".to_string(),
                                other => format!("Forgot {other} remembered values."),
                            },
                        );
                    }
                    Err(err) => {
                        error.set(Some(("We could not reset this workflow.", err.to_string())))
                    }
                }

                busy.set(false);
            });
        })
    };

    let on_action = {
        let (on_trigger, on_delete, confirming_reset) = (
            on_trigger.clone(),
            on_delete.clone(),
            confirming_reset.clone(),
        );

        Callback::from(move |action: String| match action.as_str() {
            "trigger" => on_trigger.emit(()),
            "reset" => confirming_reset.set(true),
            "delete" => on_delete.emit(()),
            _ => {}
        })
    };

    let on_expand = {
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| expanded.set(!*expanded))
    };

    // The whole row responds, not just its title. A row is a thing on a page
    // rather than a link in a sentence, and having to find the few words that
    // happen to be clickable is a worse job than it looks.
    let on_row = {
        let expanded = expanded.clone();
        Callback::from(move |event: MouseEvent| {
            if !from_a_control(&event) {
                expanded.set(!*expanded);
            }
        })
    };

    let on_cancel_reset = {
        let confirming_reset = confirming_reset.clone();
        Callback::from(move |_| confirming_reset.set(false))
    };

    let on_cancel = {
        let editing = editing.clone();
        Callback::from(move |_| editing.set(false))
    };

    let schedule = workflow
        .schedule
        .as_deref()
        .and_then(crate::util::describe_cron)
        .or_else(|| workflow.schedule.clone())
        .unwrap_or_else(|| "when its webhook is called".to_string());

    // Everything the row can do other than the one the button itself carries
    // out. Assembled rather than written out, because which of them apply
    // depends on the workflow: only a scheduled one can be run on demand, and
    // only one that remembers something can forget it.
    let mut actions = Vec::new();

    if let Some(descriptor) = &props.descriptor
        && matches!(descriptor.trigger, WorkflowTrigger::Cron { .. })
    {
        actions.push(MenuButtonOption::new("trigger", "Run now"));
    }

    if workflow.resettable {
        actions.push(MenuButtonOption::new("reset", "Reset state"));
    }

    actions.push(MenuButtonOption::new("delete", "Delete").destructive());

    // Nothing to open for a workflow that has neither an address nor a run
    // behind it, so it is not made to look as though there is.
    let expandable = workflow.health.is_some() || workflow.webhook_path.is_some();
    let panel_id = format!("workflow-detail-{}", workflow.id);

    let title = html! {
        <>
            <span class="workflow__name">{ &workflow.name }</span>
            <span class="workflow__meta">
                { &workflow.type_id }{ " · " }{ schedule }
                if workflow.enabled && let Some(next) = workflow.next_run {
                    { " · next " }{ crate::util::short_relative(next) }
                } else if !workflow.enabled {
                    { " · paused" }
                }
            </span>
        </>
    };

    html! {
        <li class={classes!("workflow", expandable.then_some("workflow--expandable"))}>
            <div class="workflow__summary" onclick={expandable.then_some(on_row)}>
                <Switch
                    id={format!("workflow-enabled-{}", workflow.id)}
                    checked={workflow.enabled}
                    onchange={on_toggle}
                    disabled={*busy}
                />

                // A real button, so the row can be opened from the keyboard and
                // says whether it is open. The rest of the row is a convenience
                // on top of this rather than the only way in.
                if expandable {
                    <button
                        type="button"
                        class="workflow__detail workflow__detail--button"
                        aria-expanded={expanded.to_string()}
                        aria-controls={panel_id.clone()}
                        onclick={on_expand}
                    >
                        { title }
                        <span
                            class={classes!(
                                "workflow__chevron",
                                expanded.then_some("workflow__chevron--open"),
                            )}
                            aria-hidden="true"
                        >
                            <svg viewBox="0 0 24 24" width="12" height="12" fill="none"
                                stroke="currentColor" stroke-width="2" stroke-linecap="round"
                                stroke-linejoin="round">
                                <polyline points="6 9 12 15 18 9" />
                            </svg>
                        </span>
                    </button>
                } else {
                    <div class="workflow__detail">{ title }</div>
                }

                if let Some(health) = &workflow.health {
                    { health_pill(health) }
                }

                // Editing is what a row is usually clicked for, so it stays a
                // button; the rest sit behind the chevron, which keeps the
                // destructive one from being a neighbour of the ordinary one.
                // A workflow whose type has gone cannot be edited, so it has no
                // default action and the menu is the whole control.
                if props.descriptor.is_some() {
                    <MenuButton
                        label={if *editing { "Close" } else { "Edit" }}
                        menu_label="Workflow actions"
                        onclick={on_edit}
                        options={actions}
                        onselect={on_action}
                        disabled={*busy}
                    />
                } else {
                    <MenuButton
                        label="Actions"
                        options={actions}
                        onselect={on_action}
                        disabled={*busy}
                    />
                }
            </div>

            if let Some(message) = (*notice).clone() {
                <p class="workflow__notice" role="status">{ message }</p>
            }

            if let Some((title, message)) = (*error).clone() {
                <Alert kind={AlertKind::Error} title={title} message={message} />
            }

            if *confirming_reset {
                <div class="workflow__confirm">
                    <p class="workflow__warning">
                        { "This forgets where the workflow got to. The next run treats \
                           everything it finds as new, so a backlog that was already dealt \
                           with will be filed again." }
                    </p>

                    <div class="workflow__confirm-actions">
                        <Button kind={ButtonKind::Danger} onclick={on_reset} busy={*busy}>
                            { "Reset state" }
                        </Button>
                        <Button
                            kind={ButtonKind::Subtle}
                            onclick={on_cancel_reset}
                            disabled={*busy}
                        >
                            { "Leave it as it is" }
                        </Button>
                    </div>
                </div>
            }

            if *expanded {
                <div class="workflow__panel" id={panel_id}>
                    if let Some(path) = workflow.webhook_path.clone() {
                        <WebhookAddress
                            workflow={workflow.id.to_string()}
                            path={path}
                            on_rotated={props.on_changed.clone()}
                        />
                    }

                    if workflow.health.is_some() {
                        <WorkflowRuns workflow={workflow.id.to_string()} />
                    }
                </div>
            }

            if props.descriptor.is_none() {
                <p class="workflow__orphaned">
                    { "This workflow's type is no longer available, so it cannot be edited or run. \
                       It is shown so you can see why it stopped, and delete it when you are ready." }
                </p>
            } else if *editing && let Some(descriptor) = props.descriptor.clone() {
                <WorkflowForm
                    descriptor={descriptor}
                    connections={props.connections.clone()}
                    initial={WorkflowValues {
                        config: workflow.config.clone(),
                        schedule: workflow.schedule.clone(),
                        enabled: workflow.enabled,
                    }}
                    submit_label="Save changes"
                    busy={*busy}
                    onsubmit={on_save}
                    oncancel={Some(on_cancel)}
                />
            }
        </li>
    }
}

#[derive(Properties, PartialEq)]
struct WorkflowRunsProps {
    workflow: String,
}

/// What a workflow's most recent runs did, and what they did it to.
///
/// Two runs at most: the last one, and the last one that failed. A history
/// would be the thing this arrangement replaced — but a failure that has been
/// overwritten by three hundred successful deliveries is a failure nobody can
/// look into, which is why the second one is kept.
#[function_component(WorkflowRuns)]
fn workflow_runs(props: &WorkflowRunsProps) -> Html {
    let state = use_state(|| None::<Option<RunState>>);
    let error = use_state(|| None::<String>);

    {
        let (id, state, error) = (props.workflow.clone(), state.clone(), error.clone());
        use_effect_with(props.workflow.clone(), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                match api::workflow_runs(&id).await {
                    Ok(found) => state.set(Some(found)),
                    Err(err) => error.set(Some(err.to_string())),
                }
            });
            || ()
        });
    }

    if let Some(message) = (*error).clone() {
        return html! {
            <Alert
                kind={AlertKind::Error}
                title="We could not load this workflow's runs."
                message={message}
            />
        };
    }

    let body = match &*state {
        None => html! { <p class="workflow-runs__empty">{ "Loading…" }</p> },
        Some(None) => html! {
            <p class="workflow-runs__empty">{ "This workflow has not run yet." }</p>
        },
        Some(Some(state)) => {
            // Shown once when the last run is itself the failure, which is the
            // usual case for a workflow that is currently broken.
            let earlier_failure = state
                .last_failure
                .as_ref()
                .filter(|failure| failure.finished_at != state.last.finished_at);

            html! {
                <>
                    <Run label="Last run" report={state.last.clone()} />
                    if let Some(failure) = earlier_failure {
                        <Run label="Last failure" report={failure.clone()} />
                    }
                </>
            }
        }
    };

    html! { <div class="workflow-runs">{ body }</div> }
}

#[derive(Properties, PartialEq)]
struct RunProps {
    label: &'static str,
    report: automate_api::RunReport,
}

#[function_component(Run)]
fn run(props: &RunProps) -> Html {
    let report = &props.report;
    let failed = report.outcome == RunOutcome::Failed;
    let took = (report.finished_at - report.started_at)
        .num_milliseconds()
        .max(0);

    html! {
        <div class="workflow-runs__run">
            <div class="workflow-runs__header">
                <span class="workflow-runs__label">{ props.label }</span>
                <StatusPill
                    tone={if failed { StatusTone::Error } else { StatusTone::Ok }}
                    label={if failed { "Failed" } else { "Succeeded" }}
                />
                <span
                    class="workflow-runs__when"
                    title={format_iso8601(report.finished_at)}
                >
                    { short_relative(report.finished_at) }{ format!(" · took {took}ms") }
                </span>
            </div>

            if let Some(message) = &report.message {
                <p class="workflow-runs__message">{ message }</p>
            }

            // The payload is what makes a failure actionable: which delivery,
            // which fields, which of the twelve repositories.
            if let Some(input) = &report.input {
                <details class="workflow-runs__input">
                    <summary>{ "What it ran on" }</summary>
                    <JsonHighlight value={input.clone()} />
                </details>
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct AddWorkflowProps {
    descriptor: WorkflowTypeDescriptor,
    connections: Vec<ConnectionSummary>,
    on_created: Callback<()>,
    oncancel: Callback<()>,
}

#[function_component(AddWorkflow)]
fn add_workflow(props: &AddWorkflowProps) -> Html {
    let error = use_state(|| None::<String>);
    let busy = use_state(|| false);

    let on_submit = {
        let (type_id, error, busy, on_created) = (
            props.descriptor.id.clone(),
            error.clone(),
            busy.clone(),
            props.on_created.clone(),
        );

        Callback::from(move |values: WorkflowValues| {
            let (type_id, error, busy, on_created) = (
                type_id.clone(),
                error.clone(),
                busy.clone(),
                on_created.clone(),
            );

            wasm_bindgen_futures::spawn_local(async move {
                busy.set(true);
                error.set(None);

                match api::create_workflow(
                    &type_id,
                    &values.config,
                    values.schedule.as_deref(),
                    values.enabled,
                )
                .await
                {
                    Ok(_) => on_created.emit(()),
                    Err(err) => error.set(Some(err.to_string())),
                }

                busy.set(false);
            });
        })
    };

    html! {
        <div class="form-modal" role="dialog" aria-modal="true" aria-labelledby="add-workflow-title">
                    <div class="form-modal__panel">
                        <div class="form-modal__header">
                            <h3 id="add-workflow-title" class="form-modal__title">
                                { format!("Add {} workflow", props.descriptor.name) }
                            </h3>
                        </div>

                        <div class="form-modal__body form-modal__body--workflow">
                            <p class="workflows__form-description">{ props.descriptor.description.clone() }</p>

                            <Documentation markdown={props.descriptor.documentation.clone()} />

                            if let Some(message) = (*error).clone() {
                                <Alert
                                    kind={AlertKind::Error}
                                    title="We could not save this workflow."
                                    message={message}
                                />
                            }

                            <WorkflowForm
                                descriptor={props.descriptor.clone()}
                                connections={props.connections.clone()}
                                initial={None::<WorkflowValues>}
                                submit_label="Add workflow"
                                busy={*busy}
                                onsubmit={on_submit}
                                oncancel={props.oncancel.clone()}
                            />
                        </div>
                    </div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct WorkflowFormProps {
    descriptor: WorkflowTypeDescriptor,
    connections: Vec<ConnectionSummary>,

    /// The values to start from. Absent for a new workflow, which starts from
    /// whatever defaults its type declares.
    #[prop_or_default]
    initial: Option<WorkflowValues>,

    submit_label: AttrValue,
    busy: bool,
    onsubmit: Callback<WorkflowValues>,

    /// Offered when there is something to go back to, which a new workflow has
    /// not got.
    #[prop_or_default]
    oncancel: Option<Callback<()>>,
}

#[function_component(WorkflowForm)]
fn workflow_form(props: &WorkflowFormProps) -> Html {
    let initial = props.initial.clone();
    let descriptor = props.descriptor.clone();

    let config = use_state(|| {
        Rc::new(
            initial
                .as_ref()
                .map(|values| values.config.clone())
                .unwrap_or_else(|| defaults_of(&descriptor)),
        )
    });

    let schedule = use_state(|| {
        props
            .initial
            .as_ref()
            .and_then(|values| values.schedule.clone())
            .or_else(|| match &props.descriptor.trigger {
                WorkflowTrigger::Cron { default_schedule } => Some(default_schedule.clone()),
                _ => None,
            })
            .unwrap_or_default()
    });

    let enabled = use_state(|| {
        props
            .initial
            .as_ref()
            .map(|values| values.enabled)
            .unwrap_or(true)
    });

    let options = use_state(FetchedOptions::new);

    // Fetch the choices for every picker whose scoping fields are filled in.
    // Keyed on those values, so choosing a different account or project reloads
    // the lists rather than leaving the previous one's on show.
    {
        let (descriptor, config, options) =
            (props.descriptor.clone(), config.clone(), options.clone());

        let dependencies: Vec<(String, String, String, Option<String>)> = descriptor
            .fields
            .iter()
            .filter_map(|field| match &field.kind {
                FieldKind::Options {
                    source,
                    depends_on,
                    parent,
                } => {
                    let connection = value_at(&config, depends_on)?.as_str()?.to_string();
                    let parent = parent
                        .as_ref()
                        .and_then(|path| value_at(&config, path))
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string());

                    Some((field.name.clone(), source.clone(), connection, parent))
                }
                _ => None,
            })
            .collect();

        use_effect_with(dependencies, move |dependencies| {
            let dependencies = dependencies.clone();

            wasm_bindgen_futures::spawn_local(async move {
                let mut fetched = FetchedOptions::new();

                for (field, source, connection, parent) in dependencies {
                    if let Ok(items) =
                        api::list_connection_options(&connection, &source, parent.as_deref()).await
                    {
                        fetched.insert(field, items);
                    }
                }

                options.set(fetched);
            });

            || ()
        });
    }

    let on_config = {
        let config = config.clone();
        Callback::from(move |next: serde_json::Value| config.set(Rc::new(next)))
    };

    let on_schedule = {
        let schedule = schedule.clone();
        Callback::from(move |value: String| schedule.set(value))
    };

    let on_enabled = {
        let enabled = enabled.clone();
        Callback::from(move |value: bool| enabled.set(value))
    };

    let on_submit = {
        let (config, schedule, enabled, onsubmit) = (
            config.clone(),
            schedule.clone(),
            enabled.clone(),
            props.onsubmit.clone(),
        );

        Callback::from(move |_| {
            let trimmed = schedule.trim().to_string();

            onsubmit.emit(WorkflowValues {
                config: (**config).clone(),
                schedule: (!trimmed.is_empty()).then_some(trimmed),
                enabled: *enabled,
            });
        })
    };

    let on_cancel = props
        .oncancel
        .clone()
        .map(|cancel| Callback::from(move |_: MouseEvent| cancel.emit(())));

    html! {
        <div class="workflow-form">
            <DynamicForm
                fields={props.descriptor.fields.clone()}
                config={(*config).clone()}
                onchange={on_config}
                connections={props.connections.clone()}
                options={(*options).clone()}
                disabled={props.busy}
            />

            if matches!(props.descriptor.trigger, WorkflowTrigger::Cron { .. }) {
                <Field
                    label="Schedule"
                    id="workflow-schedule"
                    required={true}
                    help={crate::util::describe_cron(&schedule)
                        .map(|described| AttrValue::from(format!("Runs {described}.")))}
                >
                    <TextInput
                        id="workflow-schedule"
                        value={(*schedule).clone()}
                        onchange={on_schedule}
                        placeholder={Some(AttrValue::from("@daily"))}
                        disabled={props.busy}
                    />
                </Field>
            }

            <Field
                label="Enabled"
                id="workflow-enabled"
                help={Some(AttrValue::from(
                    "A paused workflow keeps its settings and its history, and simply stops running.",
                ))}
            >
                <Switch
                    id="workflow-enabled"
                    checked={*enabled}
                    onchange={on_enabled}
                    disabled={props.busy}
                />
            </Field>

            <div class="workflow-form__actions">
                <Button kind={ButtonKind::Primary} onclick={on_submit} busy={props.busy}>
                    { props.submit_label.clone() }
                </Button>

                if let Some(on_cancel) = on_cancel {
                    <Button kind={ButtonKind::Subtle} onclick={on_cancel} disabled={props.busy}>
                        { "Cancel" }
                    </Button>
                }
            </div>
        </div>
    }
}

/// The configuration a new workflow of this type starts from.
fn defaults_of(descriptor: &WorkflowTypeDescriptor) -> serde_json::Value {
    let mut config = serde_json::json!({});

    for field in &descriptor.fields {
        if let Some(default) = &field.default {
            set_at(&mut config, &field.name, Some(default.clone()));
        }
    }

    config
}
