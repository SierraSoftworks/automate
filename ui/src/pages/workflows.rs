//! Creating and managing workflows.
//!
//! The forms on this page are not written here. The agent describes what each
//! workflow type needs collected and [`DynamicForm`] draws it, so a workflow
//! type added to the agent appears here without this file changing.

use std::rc::Rc;

use automate_api::{ConnectionSummary, FieldKind, Workflow, WorkflowTypeDescriptor};
use yew::prelude::*;

use crate::api;
use crate::components::dynamic_form::value_at;
use crate::components::{
    Alert, AlertKind, Button, ButtonKind, DynamicForm, Field, FetchedOptions, Select, SelectOption,
    TextInput,
};

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

                // The types and the linked accounts are what the form is drawn
                // from, so a failure to load either is reported by the form
                // being unavailable rather than as an error over the list.
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
    on_changed: Callback<()>,
}

#[function_component(WorkflowRow)]
fn workflow_row(props: &WorkflowRowProps) -> Html {
    let busy = use_state(|| false);
    let workflow = &props.workflow;

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

    let schedule = workflow
        .schedule
        .as_deref()
        .and_then(crate::util::describe_cron)
        .or_else(|| workflow.schedule.clone())
        .unwrap_or_else(|| "when triggered".to_string());

    html! {
        <li class="workflow">
            <div class="workflow__detail">
                <span class="workflow__name">{ &workflow.name }</span>
                <span class="workflow__meta">
                    { &workflow.type_id }{ " · " }{ schedule }
                    if let Some(next) = workflow.next_run {
                        { " · next " }{ crate::util::short_relative(next) }
                    }
                </span>
            </div>

            if !workflow.enabled {
                <span class="workflow__status workflow__status--paused">{ "Paused" }</span>
            }

            <Button kind={ButtonKind::Danger} onclick={on_delete} busy={*busy}>
                { "Delete" }
            </Button>
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
    let config = use_state(|| Rc::new(serde_json::json!({})));
    let schedule = use_state(String::new);
    let options = use_state(FetchedOptions::new);
    let error = use_state(|| None::<String>);
    let busy = use_state(|| false);

    let descriptor = chosen
        .as_ref()
        .and_then(|id| props.types.iter().find(|t| &t.id == id))
        .cloned();

    // Fetch the choices for every picker whose scoping fields are filled in.
    // Keyed on those values, so choosing a different account or project reloads
    // the lists rather than leaving the previous account's projects on show.
    {
        let (descriptor, config, options) = (descriptor.clone(), config.clone(), options.clone());

        let dependencies: Vec<(String, String, Option<String>)> = descriptor
            .as_ref()
            .map(|descriptor| {
                descriptor
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

                            Some((field.name.clone(), format!("{source}|{connection}"), parent))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        use_effect_with(dependencies.clone(), move |dependencies| {
            let dependencies = dependencies.clone();

            wasm_bindgen_futures::spawn_local(async move {
                let mut fetched = FetchedOptions::new();

                for (field, key, parent) in dependencies {
                    let Some((source, connection)) = key.split_once('|') else {
                        continue;
                    };

                    if let Ok(items) =
                        api::list_connection_options(connection, source, parent.as_deref()).await
                    {
                        fetched.insert(field, items);
                    }
                }

                options.set(fetched);
            });

            || ()
        });
    }

    let on_type = {
        let (chosen, config, schedule, error) = (
            chosen.clone(),
            config.clone(),
            schedule.clone(),
            error.clone(),
        );
        let types = props.types.clone();

        Callback::from(move |value: Option<String>| {
            // Starting a different kind of workflow starts a different form, so
            // the values collected for the last one are dropped rather than
            // carried into fields that happen to share a name.
            let defaults = value
                .as_ref()
                .and_then(|id| types.iter().find(|t| &t.id == id))
                .map(|descriptor| {
                    let mut config = serde_json::json!({});
                    for field in &descriptor.fields {
                        if let Some(default) = &field.default {
                            crate::components::dynamic_form::set_at(
                                &mut config,
                                &field.name,
                                Some(default.clone()),
                            );
                        }
                    }
                    config
                })
                .unwrap_or_else(|| serde_json::json!({}));

            let default_schedule = value
                .as_ref()
                .and_then(|id| types.iter().find(|t| &t.id == id))
                .and_then(|descriptor| match &descriptor.trigger {
                    automate_api::WorkflowTrigger::Cron { default_schedule } => {
                        Some(default_schedule.clone())
                    }
                    _ => None,
                })
                .unwrap_or_default();

            config.set(Rc::new(defaults));
            schedule.set(default_schedule);
            error.set(None);
            chosen.set(value);
        })
    };

    let on_config = {
        let config = config.clone();
        Callback::from(move |next: serde_json::Value| config.set(Rc::new(next)))
    };

    let on_schedule = {
        let schedule = schedule.clone();
        Callback::from(move |value: String| schedule.set(value))
    };

    let on_save = {
        let (chosen, config, schedule, error, busy, on_created) = (
            chosen.clone(),
            config.clone(),
            schedule.clone(),
            error.clone(),
            busy.clone(),
            props.on_created.clone(),
        );

        Callback::from(move |_| {
            let Some(type_id) = (*chosen).clone() else {
                return;
            };

            let (config, schedule, error, busy, chosen, on_created) = (
                (**config).clone(),
                (*schedule).clone(),
                error.clone(),
                busy.clone(),
                chosen.clone(),
                on_created.clone(),
            );

            wasm_bindgen_futures::spawn_local(async move {
                busy.set(true);
                error.set(None);

                let schedule = (!schedule.trim().is_empty()).then(|| schedule.trim().to_string());

                match api::create_workflow(&type_id, &config, schedule.as_deref(), true).await {
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
                <p class="workflows__form-description">{ &descriptor.description }</p>

                <DynamicForm
                    fields={descriptor.fields.clone()}
                    config={(*config).clone()}
                    onchange={on_config}
                    connections={props.connections.clone()}
                    options={(*options).clone()}
                    disabled={*busy}
                />

                if matches!(descriptor.trigger, automate_api::WorkflowTrigger::Cron { .. }) {
                    <Field
                        label="Schedule"
                        id="workflow-schedule"
                        required={true}
                        help={crate::util::describe_cron(&schedule).map(|described| AttrValue::from(format!("Runs {described}.")))}
                    >
                        <TextInput
                            id="workflow-schedule"
                            value={(*schedule).clone()}
                            onchange={on_schedule}
                            placeholder={Some(AttrValue::from("@daily"))}
                            disabled={*busy}
                        />
                    </Field>
                }

                if let Some(message) = (*error).clone() {
                    <Alert
                        kind={AlertKind::Error}
                        title="We could not save this workflow."
                        message={message}
                    />
                }

                <Button kind={ButtonKind::Primary} onclick={on_save} busy={*busy}>
                    { "Add workflow" }
                </Button>
            }
        </div>
    }
}
