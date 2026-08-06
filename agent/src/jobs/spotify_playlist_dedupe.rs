use std::collections::{HashMap, HashSet};

use automate_api::ConnectionId;

use crate::filter::FilterValue;
use crate::prelude::*;
use crate::publishers::spotify::{
    SpotifyClient, SpotifyPlaylist, SpotifyPlaylistItem, SpotifyPlaylistItemRemoval,
};

use super::SPOTIFY_PROVIDER;

/// Which linked Spotify account's playlists are tidied, and which of them.
#[derive(Clone, Serialize, Deserialize)]
pub struct SpotifyPlaylistDedupeConfig {
    pub connection: ConnectionId,

    #[serde(default)]
    pub filter: Filter,
}

impl std::fmt::Display for SpotifyPlaylistDedupeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "spotify-playlist-dedupe/{}", self.connection)
    }
}

#[derive(Clone)]
pub struct SpotifyPlaylistDedupeWorkflow;

/// The setup notes shown while somebody is configuring one of these.
const DOCUMENTATION: &str = r#"## What this does

Goes through your Spotify playlists and removes the repeated songs, leaving the
copy that sits closest to the top of the playlist. The order of everything else
is untouched, so a playlist you have arranged by hand stays arranged.

Two entries count as the same song when they are the same Spotify track. The
same recording released on both an album and a single is two different tracks to
Spotify, and this leaves both alone — it removes the copies you added twice, not
the ones that merely sound alike.

Playlists you follow but do not own are skipped, along with local files and
entries Spotify can no longer resolve.

## Choosing which playlists

Without a filter this tidies every playlist you can write to, which is usually
what you want. The filter runs against each playlist and can match on `name`,
`id`, `owner`, `public` and `collaborative`:

```
name startswith "Road Trip"
```

Collaborative playlists are the ones worth thinking about before including them.
A duplicate there may be somebody else's addition, and removing it is a change
they did not ask for:

```
collaborative == false
```

## Permissions

Reading your private playlists needs the `playlist-read-private` scope (and
`playlist-read-collaborative` for the shared ones), and removing songs needs
`playlist-modify-private` and `playlist-modify-public`. If your Spotify
connection was linked before those were requested, reconnect the account so the
new authorisation covers them.

## Scheduling

This reads every playlist in the account on each run, which is a lot to ask of a
rate-limited API, and duplicates are not urgent. `@weekly` is plenty; a failed
run backs off for an hour before it tries again.
"#;

crate::register_job!(SpotifyPlaylistDedupeWorkflow);
crate::register_workflow_type!(SpotifyPlaylistDedupeWorkflow);

impl Filterable for SpotifyPlaylist {
    fn get(&self, key: &str) -> FilterValue<'_> {
        match key {
            "id" => self.id.as_str().into(),
            "name" => self.name.as_str().into(),
            "owner" => self.owner.id.as_str().into(),
            // A playlist Spotify reports no visibility for is not a public one.
            "public" => FilterValue::Bool(self.public.unwrap_or(false)),
            "collaborative" => FilterValue::Bool(self.collaborative),
            _ => FilterValue::Null,
        }
    }
}

impl crate::workflows::ConfigurableWorkflow for SpotifyPlaylistDedupeWorkflow {
    type ConfigType = SpotifyPlaylistDedupeConfig;

    fn type_id() -> &'static str {
        "spotify-playlist-dedupe"
    }

    fn descriptor() -> automate_api::WorkflowTypeDescriptor {
        use automate_api::{
            ConnectionKind, FieldDescriptor, FieldKind, WorkflowTrigger, WorkflowTypeDescriptor,
        };

        WorkflowTypeDescriptor {
            id: <Self as crate::workflows::ConfigurableWorkflow>::type_id().to_string(),
            name: "Spotify Playlist De-duplication".to_string(),
            description: "Removes repeated songs from your playlists, keeping the first copy."
                .to_string(),
            documentation: DOCUMENTATION.to_string(),
            trigger: WorkflowTrigger::Cron {
                default_schedule: "@weekly".to_string(),
            },
            fields: vec![
                FieldDescriptor::new(
                    crate::config_path!(SpotifyPlaylistDedupeConfig: connection),
                    "Spotify account",
                    FieldKind::Connection {
                        provider: SPOTIFY_PROVIDER.to_string(),
                        connection_kind: Some(ConnectionKind::OAuth2),
                    },
                )
                .with_help("Whose playlists are tidied.")
                .required(),
                FieldDescriptor::new(
                    crate::config_path!(SpotifyPlaylistDedupeConfig: filter),
                    "Filter",
                    FieldKind::Filter {
                        fields: vec![
                            "id".into(),
                            "name".into(),
                            "owner".into(),
                            "public".into(),
                            "collaborative".into(),
                        ],
                    },
                )
                .with_help(
                    "Only tidy playlists matching this, such as `collaborative == false`. \
                     Leave it empty to tidy every playlist you can write to.",
                ),
            ],
        }
    }
}

/// The later copies of every repeated item, in the order they were first
/// repeated.
///
/// Positions rather than URIs alone, because Spotify removes every copy of a
/// URI it is handed: keeping the earliest entry means naming the ones to drop.
/// The positions are indices into the playlist as read, including the entries
/// skipped here, which is what Spotify resolves them against.
fn duplicates(items: &[SpotifyPlaylistItem]) -> Vec<SpotifyPlaylistItemRemoval> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut removals: Vec<SpotifyPlaylistItemRemoval> = Vec::new();
    let mut removal_of: HashMap<&str, usize> = HashMap::new();

    for (position, item) in items.iter().enumerate() {
        // A local file is addressed by a URI Spotify will not take back, and an
        // item with no track is one it could not resolve at all.
        if item.is_local {
            continue;
        }

        let Some(uri) = item.track.as_ref().map(|track| track.uri.as_str()) else {
            continue;
        };

        if seen.insert(uri) {
            continue;
        }

        match removal_of.get(uri) {
            Some(&at) => removals[at].positions.push(position),
            None => {
                removal_of.insert(uri, removals.len());
                removals.push(SpotifyPlaylistItemRemoval {
                    uri: uri.to_string(),
                    positions: vec![position],
                });
            }
        }
    }

    removals
}

/// How many removals Spotify accepts in one request.
const REMOVAL_BATCH: usize = 100;

impl Job for SpotifyPlaylistDedupeWorkflow {
    type JobType = SpotifyPlaylistDedupeConfig;

    fn partition() -> &'static str {
        "spotify/playlist-dedupe"
    }

    /// Visibility timeout / retry backoff. Calls the rate-limited Spotify API,
    /// so a failed run waits an hour before retrying.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::hours(1)
    }

    fn propagate_parent() -> bool {
        false
    }

    #[instrument("workflow.spotify_playlist_dedupe.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();

        // The refresh token has expired or been revoked: a re-authorization
        // reminder has been raised, so completing here (rather than erroring)
        // stops us retrying against a dead account on every schedule.
        let Some(token) =
            crate::connections::resolve_oauth2_token(job.connection, SPOTIFY_PROVIDER, services)
                .await?
        else {
            return Ok(());
        };

        let client = SpotifyClient::new(token, services.http_client());
        let user = client.get_current_user().await?;

        for playlist in client.get_playlists().await? {
            // Playlists this account only follows cannot be written to, and
            // asking would earn a rejection for every one of them.
            if playlist.owner.id != user.id && !playlist.collaborative {
                continue;
            }

            if !job.filter.matches(&playlist)? {
                continue;
            }

            let items = client.get_playlist_items(&playlist.id).await?;
            let removals = duplicates(&items);

            if removals.is_empty() {
                continue;
            }

            info!(
                spotify.playlist = %playlist.name,
                spotify.duplicates = removals.iter().map(|r| r.positions.len()).sum::<usize>(),
                "Removing duplicate songs from '{}'.",
                playlist.name
            );

            for batch in removals.chunks(REMOVAL_BATCH) {
                client
                    .remove_playlist_items(&playlist.id, &playlist.snapshot_id, batch)
                    .await?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::ConfigurableWorkflow;

    fn item(uri: &str) -> SpotifyPlaylistItem {
        serde_json::from_value(serde_json::json!({ "track": { "uri": uri } })).unwrap()
    }

    #[test]
    fn the_copy_nearest_the_top_of_the_playlist_is_the_one_kept() {
        let items = [item("a"), item("b"), item("a"), item("c"), item("a")];

        assert_eq!(
            duplicates(&items),
            vec![SpotifyPlaylistItemRemoval {
                uri: "a".to_string(),
                positions: vec![2, 4],
            }],
        );
    }

    #[test]
    fn a_playlist_without_repeats_asks_for_nothing_to_be_removed() {
        let items = [item("a"), item("b"), item("c")];

        assert!(duplicates(&items).is_empty());
    }

    /// Both are entries Spotify will not accept a removal for, and both still
    /// occupy a position, so the positions of everything after them have to
    /// account for them.
    #[test]
    fn local_files_and_unresolvable_entries_are_left_where_they_are() {
        let items: Vec<SpotifyPlaylistItem> = serde_json::from_value(serde_json::json!([
            { "track": { "uri": "spotify:local:mine" }, "is_local": true },
            { "track": null },
            { "track": { "uri": "spotify:local:mine" }, "is_local": true },
            { "track": { "uri": "a" } },
            { "track": { "uri": "a" } },
        ]))
        .unwrap();

        assert_eq!(
            duplicates(&items),
            vec![SpotifyPlaylistItemRemoval {
                uri: "a".to_string(),
                positions: vec![4],
            }],
        );
    }

    #[test]
    fn a_configuration_naming_only_a_connection_tidies_every_playlist() {
        let config: SpotifyPlaylistDedupeConfig = serde_json::from_value(serde_json::json!({
            "connection": ConnectionId::from_entropy(0).to_string(),
        }))
        .unwrap();

        let playlist: SpotifyPlaylist = serde_json::from_value(serde_json::json!({
            "id": "1",
            "name": "Road Trip",
            "uri": "spotify:playlist:1",
            "collaborative": false,
            "owner": { "id": "alice" },
            "snapshot_id": "snap",
        }))
        .unwrap();

        assert!(config.filter.matches(&playlist).unwrap());
    }

    #[test]
    fn de_duplication_requires_a_spotify_oauth_connection() {
        let descriptor = SpotifyPlaylistDedupeWorkflow::descriptor();
        let connection = descriptor
            .fields
            .iter()
            .find(|field| field.name == "connection")
            .unwrap();

        assert!(connection.required);
        assert!(matches!(
            &connection.kind,
            automate_api::FieldKind::Connection {
                provider,
                connection_kind: Some(automate_api::ConnectionKind::OAuth2),
            } if provider == SPOTIFY_PROVIDER
        ));
    }
}
