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
    ConnectionSummary, FieldKind, Workflow, WorkflowTrigger, WorkflowTypeDescriptor,
};
use yew::prelude::*;

use crate::api;
use crate::components::dynamic_form::{set_at, value_at};
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, Documentation, DynamicForm, Field, FetchedOptions, Select,
    SelectOption,
    Switch, TextInput, WebhookAddress,
};

/// What a form hands back when it is submitted.
#[derive(Clone, PartialEq)]
pub struct WorkflowValues {
    pub config: serde_json::Value,
    pub schedule: Option<String>,
    pub enabled: bool,
}

#[function_component(Workflows)]
pub fn workflows() -> Html {
    let workflows = use_state(Vec::<Workflow>::new);
    let types = use_state(Vec::<WorkflowTypeDescriptor>::new);
    let connections = use_state(Vec::<ConnectionSummary>::new);
    let error = use_state(|| None::<String>);
    let loading = use_state(|| true);
    let reload = use_state(|| 0u32);

    {
        let (workflows, types, connections, error, loading) = (
            workflows.clone(),
            types.clone(),
            connections.clone(),
            error.clone(),
            loading.clone(),
        );

        use_effect_with(*reload, move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                loading.set(true);

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

    let body = if *loading {
        html! { <p class="workflows__empty">{ "Loading…" }</p> }
    } else if workflows.is_empty() {
        html! {
            <p class="workflows__empty">
                { "You have no workflows yet. Add one below to have Automate watch something for you." }
            </p>
        }
    } else {
        html! {
            <ul class="workflows__list">
                { for workflows.iter().map(|workflow| html! {
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
            <h2 class="workflows__title">{ "Workflows" }</h2>

            if let Some(message) = (*error).clone() {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not load your workflows."
                    message={message}
                />
            }

            { body }

            <AddWorkflow
                types={(*types).clone()}
                connections={(*connections).clone()}
                on_created={on_changed}
            />
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
    let error = use_state(|| None::<String>);
    let workflow = &props.workflow;

    /// Saves a change to this workflow, whatever prompted it.
    fn save(
        id: String,
        values: WorkflowValues,
        busy: UseStateHandle<bool>,
        error: UseStateHandle<Option<String>>,
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
                Err(err) => error.set(Some(err.to_string())),
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

    html! {
        <li class="workflow">
            <div class="workflow__summary">
                <Switch
                    id={format!("workflow-enabled-{}", workflow.id)}
                    checked={workflow.enabled}
                    onchange={on_toggle}
                    disabled={*busy}
                />

                <div class="workflow__detail">
                    <span class="workflow__name">{ &workflow.name }</span>
                    <span class="workflow__meta">
                        { &workflow.type_id }{ " · " }{ schedule }
                        if workflow.enabled && let Some(next) = workflow.next_run {
                            { " · next " }{ crate::util::short_relative(next) }
                        } else if !workflow.enabled {
                            { " · paused" }
                        }
                    </span>
                </div>

                if props.descriptor.is_some() {
                    <Button onclick={on_edit} disabled={*busy}>
                        if *editing { { "Close" } } else { { "Edit" } }
                    </Button>
                }

                <Button kind={ButtonKind::Danger} onclick={on_delete} busy={*busy}>
                    { "Delete" }
                </Button>
            </div>

            if let Some(message) = (*error).clone() {
                <Alert
                    kind={AlertKind::Error}
                    title="We could not save this change."
                    message={message}
                />
            }

            if let Some(path) = workflow.webhook_path.clone() {
                <WebhookAddress
                    workflow={workflow.id.to_string()}
                    path={path}
                    on_rotated={props.on_changed.clone()}
                />
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
struct AddWorkflowProps {
    types: Vec<WorkflowTypeDescriptor>,
    connections: Vec<ConnectionSummary>,
    on_created: Callback<()>,
}

#[function_component(AddWorkflow)]
fn add_workflow(props: &AddWorkflowProps) -> Html {
    let chosen = use_state(|| None::<String>);
    let error = use_state(|| None::<String>);
    let busy = use_state(|| false);

    let descriptor = chosen
        .as_ref()
        .and_then(|id| props.types.iter().find(|t| &t.id == id))
        .cloned();

    let on_type = {
        let (chosen, error) = (chosen.clone(), error.clone());
        Callback::from(move |value: Option<String>| {
            error.set(None);
            chosen.set(value);
        })
    };

    let on_submit = {
        let (chosen, error, busy, on_created) = (
            chosen.clone(),
            error.clone(),
            busy.clone(),
            props.on_created.clone(),
        );

        Callback::from(move |values: WorkflowValues| {
            let Some(type_id) = (*chosen).clone() else {
                return;
            };

            let (chosen, error, busy, on_created) = (
                chosen.clone(),
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
                    Ok(_) => {
                        chosen.set(None);
                        on_created.emit(());
                    }
                    Err(err) => error.set(Some(err.to_string())),
                }

                busy.set(false);
            });
        })
    };

    let choices: Vec<SelectOption> = props
        .types
        .iter()
        .map(|descriptor| SelectOption::new(descriptor.id.clone(), descriptor.name.clone()))
        .collect();

    html! {
        <div class="workflows__form">
            <h3 class="workflows__form-title">{ "Add a workflow" }</h3>

            <Field label="What should it watch?" id="workflow-type" required={true}>
                <Select
                    id="workflow-type"
                    value={(*chosen).clone().map(AttrValue::from)}
                    onchange={on_type}
                    options={choices}
                    placeholder="Choose a kind of workflow"
                    clearable={true}
                    disabled={*busy}
                />
            </Field>

            if let Some(descriptor) = descriptor {
                { render_new_form(&descriptor, props, &error, *busy, on_submit) }
            }
        </div>
    }
}

/// The form for a workflow that does not exist yet.
fn render_new_form(
    descriptor: &WorkflowTypeDescriptor,
    props: &AddWorkflowProps,
    error: &UseStateHandle<Option<String>>,
    busy: bool,
    on_submit: Callback<WorkflowValues>,
) -> Html {
    html! {
        <>
                <p class="workflows__form-description">{ descriptor.description.clone() }</p>

                <Documentation markdown={descriptor.documentation.clone()} />

                if let Some(message) = (**error).clone() {
                    <Alert
                        kind={AlertKind::Error}
                        title="We could not save this workflow."
                        message={message}
                    />
                }

                <WorkflowForm
                    // Keyed by type, so choosing a different kind of workflow
                    // starts a fresh form rather than carrying values into
                    // fields that happen to share a name.
                    key={descriptor.id.clone()}
                    descriptor={descriptor.clone()}
                    connections={props.connections.clone()}
                    initial={None::<WorkflowValues>}
                    submit_label="Add workflow"
                    busy={busy}
                    onsubmit={on_submit}
                />
        </>
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

    let on_cancel = props.oncancel.clone().map(|cancel| {
        Callback::from(move |_: MouseEvent| cancel.emit(()))
    });

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
