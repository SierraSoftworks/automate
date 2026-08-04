//! Reading and writing workflows as TOML.
//!
//! Workflows moved out of the configuration file and into the database so that
//! several people could each have their own and edit them without a restart.
//! That is right for the people using an installation and wrong for the person
//! running one, who wants their workflows reviewed, versioned and restorable
//! like anything else they operate.
//!
//! This is the bridge. The database stays the source of truth; a file is
//! something you can produce from it and apply back to it.
//!
//! # The file names the workflow
//!
//! Every exported workflow carries its identifier, and importing one uses it. A
//! file applied to an empty database therefore reproduces the same identifiers
//! rather than a fresh set, which is what makes it a restore rather than a
//! second copy of everything.
//!
//! # Applying is additive unless asked otherwise
//!
//! A workflow in the database but not in the file is left alone by default,
//! because the common case is a file describing part of an installation and the
//! surprising outcome is deleting somebody's work. `prune` asks for the other
//! reading, where the file is the whole truth and anything absent from it should
//! go.

use std::collections::{BTreeMap, HashSet};

use automate_api::WorkflowId;
use human_errors::Error;

use crate::prelude::*;
use crate::workflow_store::{WorkflowDraft, WorkflowRecord, WorkflowStore};

/// The keys an exported workflow carries that are not part of its
/// configuration.
///
/// Named here so that import and export cannot disagree about which keys are
/// the envelope and which are the contents.
const ID_KEY: &str = "id";
const SCHEDULE_KEY: &str = "cron";
const ENABLED_KEY: &str = "enabled";

/// What applying a file did.
#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct ImportSummary {
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
}

/// Renders a tenant's workflows as TOML.
///
/// Grouped by type and ordered by identifier, so that exporting the same
/// workflows twice produces the same bytes and a stored file only changes when
/// something actually did.
pub fn export(records: &[WorkflowRecord]) -> Result<String, Error> {
    let mut by_type: BTreeMap<&str, Vec<toml::Value>> = BTreeMap::new();

    let mut records: Vec<&WorkflowRecord> = records.iter().collect();
    records.sort_by_key(|record| record.id.to_string());

    for record in records {
        let mut table = match toml::Value::try_from(&record.config).wrap_system_err(
            format!(
                "We could not represent the workflow '{}' as TOML.",
                record.id
            ),
            &["Please report this issue to the dev team on GitHub."],
        )? {
            toml::Value::Table(table) => table,
            _ => {
                // A configuration is an object by construction; anything else
                // would have been refused when it was saved.
                return Err(human_errors::system(
                    format!(
                        "The workflow '{}' does not hold a set of settings, so it cannot be written out.",
                        record.id
                    ),
                    &["Please report this issue to the dev team on GitHub."],
                ));
            }
        };

        table.insert(ID_KEY.into(), record.id.to_string().into());

        if let Some(schedule) = &record.schedule {
            table.insert(SCHEDULE_KEY.into(), schedule.clone().into());
        }

        // Only written when it is not the default, so an export is not mostly
        // repetitions of `enabled = true`.
        if !record.enabled {
            table.insert(ENABLED_KEY.into(), false.into());
        }

        by_type
            .entry(record.type_id.as_str())
            .or_default()
            .push(toml::Value::Table(table));
    }

    let workflows: toml::map::Map<String, toml::Value> = by_type
        .into_iter()
        .map(|(type_id, entries)| (type_id.to_string(), toml::Value::Array(entries)))
        .collect();

    let mut root = toml::Table::new();
    root.insert("workflows".into(), toml::Value::Table(workflows));

    toml::to_string_pretty(&root).wrap_system_err(
        "We could not write these workflows out as TOML.",
        &["Please report this issue to the dev team on GitHub."],
    )
}

/// Applies a TOML document to a tenant's workflows.
pub async fn import<S: Services>(
    store: &WorkflowStore<S>,
    document: &str,
    prune: bool,
) -> Result<ImportSummary, Error> {
    let parsed: toml::Table = toml::from_str(document).map_err(|err| {
        human_errors::user(
            format!("This is not a file we could read: {err}"),
            &["Check that it is valid TOML, as produced by exporting your workflows."],
        )
    })?;

    let mut summary = ImportSummary::default();
    let mut seen: HashSet<WorkflowId> = HashSet::new();

    let Some(workflows) = parsed.get("workflows").and_then(|value| value.as_table()) else {
        // An empty file is a valid description of nothing. With `prune` that is
        // a request to delete everything, which is a thing somebody could
        // genuinely mean but almost certainly does not, so it is refused.
        if prune {
            return Err(human_errors::user(
                "This file describes no workflows, so applying it as the whole truth would delete all of them.",
                &[
                    "If you meant to remove every workflow, delete them individually.",
                    "Check that the file has a [workflows] section.",
                ],
            ));
        }

        return Ok(summary);
    };

    for (type_id, entries) in workflows {
        let Some(entries) = entries.as_array() else {
            return Err(human_errors::user(
                format!("The '{type_id}' workflows are not written as a list."),
                &["Each workflow should be its own [[workflows.<type>]] section."],
            ));
        };

        for entry in entries {
            let Some(table) = entry.as_table() else {
                return Err(human_errors::user(
                    format!("One of the '{type_id}' workflows is not a set of settings."),
                    &["Check the file against one produced by exporting your workflows."],
                ));
            };

            let (id, draft) = read_entry(type_id, table)?;

            match id {
                Some(id) => {
                    let existed = store.find(id).await?.is_some();
                    store.upsert(id, draft).await?;
                    seen.insert(id);

                    if existed {
                        summary.updated += 1;
                    } else {
                        summary.created += 1;
                    }
                }
                None => {
                    // A workflow written by hand need not have an identifier;
                    // one is chosen for it, and appears the next time the file
                    // is exported.
                    let created = store.create(draft).await?;
                    seen.insert(created.id);
                    summary.created += 1;
                }
            }
        }
    }

    if prune {
        for record in store.records().await? {
            if !seen.contains(&record.id) {
                store.delete(record.id).await?;
                summary.deleted += 1;
            }
        }
    }

    Ok(summary)
}

/// Splits one entry into the identifier it claims and the workflow it describes.
fn read_entry(
    type_id: &str,
    table: &toml::map::Map<String, toml::Value>,
) -> Result<(Option<WorkflowId>, WorkflowDraft), Error> {
    let id = match table.get(ID_KEY) {
        Some(value) => {
            let raw = value.as_str().ok_or_else(|| {
                human_errors::user(
                    format!("A '{type_id}' workflow has an id that is not written as text."),
                    &["An id looks like id = \"copper-tiger-canyon\"."],
                )
            })?;

            Some(raw.parse::<WorkflowId>().map_err(|err| {
                human_errors::user(
                    format!("A '{type_id}' workflow has an id we could not read: {err}"),
                    &["Ids are three words, such as \"copper-tiger-canyon\"."],
                )
            })?)
        }
        None => None,
    };

    let schedule = match table.get(SCHEDULE_KEY) {
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| {
                    human_errors::user(
                        format!(
                            "A '{type_id}' workflow has a schedule that is not written as text."
                        ),
                        &["A schedule looks like cron = \"@daily\"."],
                    )
                })?
                .to_string(),
        ),
        None => None,
    };

    let enabled = match table.get(ENABLED_KEY) {
        Some(value) => value.as_bool().ok_or_else(|| {
            human_errors::user(
                format!(
                    "A '{type_id}' workflow says whether it is enabled in a way we could not read."
                ),
                &["This should be enabled = true or enabled = false."],
            )
        })?,
        None => true,
    };

    // Everything that is not the envelope is the workflow's own configuration.
    let config: toml::map::Map<String, toml::Value> = table
        .iter()
        .filter(|(key, _)| ![ID_KEY, SCHEDULE_KEY, ENABLED_KEY].contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    let config = serde_json::to_value(toml::Value::Table(config)).wrap_system_err(
        format!("We could not read the settings of a '{type_id}' workflow."),
        &["Please report this issue to the dev team on GitHub."],
    )?;

    Ok((
        id,
        WorkflowDraft {
            type_id: type_id.to_string(),
            config,
            schedule,
            enabled,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ServicesContainer;

    fn draft(name: &str) -> WorkflowDraft {
        WorkflowDraft {
            type_id: "rss".into(),
            config: serde_json::json!({
                "name": name,
                "url": "https://example.com/rss/",
                "homepage": "https://example.com/",
            }),
            schedule: Some("@daily".into()),
            enabled: true,
        }
    }

    async fn new_store() -> WorkflowStore<impl Services> {
        WorkflowStore::new(ServicesContainer::new_mock().await.unwrap())
    }

    #[tokio::test]
    async fn a_file_written_out_can_be_read_back_unchanged() {
        let store = new_store().await;
        store.create(draft("Citation Needed")).await.unwrap();
        store.create(draft("Tailscale")).await.unwrap();

        let document = export(&store.records().await.unwrap()).unwrap();

        // Applied to an empty installation, the file should reproduce what it
        // came from rather than a similar-looking copy.
        let restored = new_store().await;
        let summary = import(&restored, &document, false).await.unwrap();

        assert_eq!(summary.created, 2);
        assert_eq!(summary.updated, 0);
        assert_eq!(
            export(&restored.records().await.unwrap()).unwrap(),
            document,
            "a restored installation should export byte-for-byte what it was restored from",
        );
    }

    #[tokio::test]
    async fn exporting_the_same_workflows_twice_produces_the_same_bytes() {
        // Otherwise a file kept in version control would show a change every
        // time it was regenerated, and real changes would be lost in the noise.
        let store = new_store().await;
        for name in ["A", "B", "C", "D"] {
            store.create(draft(name)).await.unwrap();
        }

        let records = store.records().await.unwrap();
        assert_eq!(export(&records).unwrap(), export(&records).unwrap());
    }

    #[tokio::test]
    async fn applying_a_file_twice_changes_nothing_the_second_time() {
        let store = new_store().await;
        store.create(draft("Citation Needed")).await.unwrap();
        let document = export(&store.records().await.unwrap()).unwrap();

        let first = import(&store, &document, false).await.unwrap();
        let second = import(&store, &document, false).await.unwrap();

        assert_eq!(first, second);
        assert_eq!(second.created, 0, "the workflow already existed");
        assert_eq!(second.updated, 1);
        assert_eq!(store.records().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_workflow_written_by_hand_without_an_id_is_given_one() {
        let store = new_store().await;

        let summary = import(
            &store,
            r#"
            [[workflows.rss]]
            name = "Written by hand"
            url = "https://example.com/rss/"
            homepage = "https://example.com/"
            cron = "@daily"
            "#,
            false,
        )
        .await
        .unwrap();

        assert_eq!(summary.created, 1);

        // And it appears in the next export, so the file can be regenerated with
        // the identifier filled in.
        let document = export(&store.records().await.unwrap()).unwrap();
        assert!(document.contains("id = "), "{document}");
    }

    #[tokio::test]
    async fn a_workflow_missing_from_the_file_is_left_alone_by_default() {
        let store = new_store().await;
        let kept = store.create(draft("Not in the file")).await.unwrap();
        store.create(draft("In the file")).await.unwrap();

        let document = export(&[store.get(kept.id).await.unwrap()]).unwrap();
        let summary = import(&store, &document, false).await.unwrap();

        assert_eq!(summary.deleted, 0);
        assert_eq!(
            store.records().await.unwrap().len(),
            2,
            "applying part of an installation should not delete the rest of it",
        );
    }

    #[tokio::test]
    async fn a_workflow_missing_from_the_file_is_removed_when_pruning() {
        let store = new_store().await;
        let kept = store.create(draft("In the file")).await.unwrap();
        store.create(draft("Not in the file")).await.unwrap();

        let document = export(&[store.get(kept.id).await.unwrap()]).unwrap();
        let summary = import(&store, &document, true).await.unwrap();

        assert_eq!(summary.deleted, 1);

        let remaining = store.records().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, kept.id);
    }

    #[tokio::test]
    async fn an_empty_file_will_not_silently_delete_everything() {
        let store = new_store().await;
        store.create(draft("Citation Needed")).await.unwrap();

        // Technically a description of no workflows, but far more likely a file
        // that failed to generate than a request to delete the lot.
        assert!(import(&store, "", true).await.is_err());
        assert_eq!(store.records().await.unwrap().len(), 1);

        // Without pruning it means nothing, and does nothing.
        assert_eq!(
            import(&store, "", false).await.unwrap(),
            ImportSummary::default(),
        );
    }

    #[tokio::test]
    async fn a_workflow_the_agent_could_not_run_is_refused() {
        let store = new_store().await;

        let Err(err) = import(
            &store,
            r#"
            [[workflows.rss]]
            name = "Missing its feed"
            homepage = "https://example.com/"
            cron = "@daily"
            "#,
            false,
        )
        .await
        else {
            panic!("a workflow whose settings will not load should not be imported");
        };

        assert!(format!("{err}").contains("url"), "{err}");
    }

    #[tokio::test]
    async fn an_unreadable_file_is_reported_rather_than_partly_applied() {
        let store = new_store().await;

        assert!(import(&store, "this is not toml", false).await.is_err());
        assert!(store.records().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_paused_workflow_survives_the_round_trip() {
        let store = new_store().await;
        let paused = store
            .create(WorkflowDraft {
                enabled: false,
                ..draft("Paused")
            })
            .await
            .unwrap();

        let document = export(&store.records().await.unwrap()).unwrap();
        assert!(document.contains("enabled = false"), "{document}");

        let restored = new_store().await;
        import(&restored, &document, false).await.unwrap();

        assert!(!restored.get(paused.id).await.unwrap().enabled);
    }
}
