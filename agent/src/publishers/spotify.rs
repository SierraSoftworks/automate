use std::borrow::Cow;

use crate::prelude::*;

pub struct SpotifyClient {
    pub api_endpoint: String,
    refresh_token: OAuth2RefreshToken,
    client: reqwest::Client,
}

impl SpotifyClient {
    pub fn new(refresh_token: OAuth2RefreshToken, http_client: reqwest::Client) -> Self {
        SpotifyClient {
            api_endpoint: "https://api.spotify.com/v1".to_string(),
            refresh_token,

            client: http_client,
        }
    }

    #[instrument("spotify.get_current_user", skip(self), err(Display))]
    pub async fn get_current_user(&self) -> Result<SpotifyUser, human_errors::Error> {
        let user: SpotifyUser = self
            .call_spotify(reqwest::Method::GET, "me", None::<()>)
            .await?;

        Ok(user)
    }

    #[instrument("spotify.get_saved_tracks", skip(self, since), err(Display))]
    pub async fn get_saved_tracks(
        &self,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<SpotifySavedTrack>, human_errors::Error> {
        let tracks = self
            .call_spotify_paginated(
                reqwest::Method::GET,
                "me/tracks",
                None::<()>,
                |item: &SpotifySavedTrack| item.added_at > since,
            )
            .await?;

        Ok(tracks)
    }

    #[instrument("spotify.get_playlists", skip(self), err(Display))]
    pub async fn get_playlists(&self) -> Result<Vec<SpotifyPlaylist>, human_errors::Error> {
        let playlists = self
            .call_spotify_paginated(reqwest::Method::GET, "me/playlists", None::<()>, |_| true)
            .await?;

        Ok(playlists)
    }

    #[instrument("spotify.create_playlist", skip(self, name, description), err(Display))]
    pub async fn create_playlist(
        &self,
        name: impl ToString,
        public: bool,
        collaborative: bool,
        description: Option<String>,
    ) -> Result<SpotifyPlaylist, human_errors::Error> {
        let user = self.get_current_user().await?;

        let playlist: SpotifyPlaylist = self
            .call_spotify(
                reqwest::Method::POST,
                format!("users/{}/playlists", user.id),
                Some(serde_json::json!({
                    "name": name.to_string(),
                    "public": public,
                    "collaborative": collaborative,
                    "description": description,
                })),
            )
            .await?;

        Ok(playlist)
    }

    #[instrument(
        "spotify.add_tracks_to_playlist",
        skip(self, playlist_id, track_uris),
        err(Display)
    )]
    pub async fn add_tracks_to_playlist(
        &self,
        playlist_id: impl ToString,
        track_uris: Vec<String>,
    ) -> Result<(), human_errors::Error> {
        let _: serde_json::Value = self
            .call_spotify(
                reqwest::Method::POST,
                format!("playlists/{}/tracks", playlist_id.to_string()),
                Some(serde_json::json!({
                    "uris": track_uris,
                    "position": 0,
                })),
            )
            .await?;

        Ok(())
    }

    #[instrument("spotify.get_playlist_items", skip(self, playlist_id), err(Display))]
    pub async fn get_playlist_items(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<SpotifyPlaylistItem>, human_errors::Error> {
        let items = self
            .call_spotify_paginated(
                reqwest::Method::GET,
                format!("playlists/{playlist_id}/tracks"),
                None::<()>,
                |_| true,
            )
            .await?;

        Ok(items)
    }

    /// Removes the named occurrences of items from a playlist.
    ///
    /// `snapshot_id` is the version the positions were read from, so Spotify
    /// resolves them against that version rather than whatever the playlist has
    /// become since — which is what makes several batches of removals, each
    /// computed from one reading, safe to send.
    #[instrument(
        "spotify.remove_playlist_items",
        skip(self, playlist_id, snapshot_id, removals),
        err(Display)
    )]
    pub async fn remove_playlist_items(
        &self,
        playlist_id: &str,
        snapshot_id: &str,
        removals: &[SpotifyPlaylistItemRemoval],
    ) -> Result<(), human_errors::Error> {
        let _: serde_json::Value = self
            .call_spotify(
                reqwest::Method::DELETE,
                format!("playlists/{playlist_id}/tracks"),
                Some(serde_json::json!({
                    "tracks": removals,
                    "snapshot_id": snapshot_id,
                })),
            )
            .await?;

        Ok(())
    }

    async fn call_spotify_paginated<T: DeserializeOwned, W: Fn(&T) -> bool>(
        &self,
        method: reqwest::Method,
        path: impl Into<Cow<'_, str>>,
        json: Option<impl serde::Serialize>,
        filter: W,
    ) -> Result<Vec<T>, human_errors::Error> {
        let mut results = Vec::new();
        let mut url = path.into().to_string();

        loop {
            let resp: PaginatedResponse<T> = self
                .call_spotify(method.clone(), url, json.as_ref())
                .await?;

            for item in resp.items.into_iter() {
                if filter(&item) {
                    results.push(item);
                } else {
                    return Ok(results);
                }
            }

            if let Some(next) = resp.next {
                url = next;
            } else {
                break;
            }
        }

        Ok(results)
    }

    async fn call_spotify<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: impl Into<Cow<'_, str>>,
        json: Option<impl Serialize>,
    ) -> Result<T, human_errors::Error> {
        let access_token = self.refresh_token.access_token();

        let path = path.into();
        let url = if path.starts_with(&self.api_endpoint) {
            path.into_owned()
        } else {
            format!("{}/{}", self.api_endpoint, path)
        };

        let req = self.client.request(method, url).bearer_auth(access_token);

        let req = if let Some(json) = json {
            req.json(&json)
        } else {
            req
        };

        let req = req
            .build()
            .or_system_err(&["Report this issue to the development team on GitHub."])?;

        let resp = self
            .client
            .execute(req)
            .await
            .or_user_err(&["Make sure that your internet connection is working."])?
            .error_for_status()
            .wrap_user_err(
                "Failed to call Spotify's API",
                &[
                    "Ensure that your internet connection is working.",
                    "Check that Spotify's service is operational.",
                ],
            )?;

        resp.json().await.or_user_err(&[
            "Ensure that your internet connection is working.",
            "Check that Spotify's service is operational.",
        ])
    }
}

#[derive(Deserialize)]
struct PaginatedResponse<T> {
    items: Vec<T>,
    next: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct SpotifyUser {
    pub id: String,
    pub display_name: Option<String>,
    pub uri: String,
}

#[derive(Deserialize)]
pub struct SpotifySavedTrack {
    pub added_at: chrono::DateTime<chrono::Utc>,
    pub track: SpotifyTrack,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct SpotifyTrack {
    pub id: String,
    pub name: String,
    pub uri: String,

    pub artists: Vec<SpotifyArtist>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct SpotifyArtist {
    pub id: String,
    pub name: String,
    pub uri: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct SpotifyPlaylist {
    pub id: String,
    pub name: String,
    pub uri: String,

    /// Null for the playlists Spotify considers the question irrelevant for,
    /// so this cannot be a bare `bool` without a listing occasionally failing
    /// to parse in its entirety.
    #[serde(default)]
    pub public: Option<bool>,

    pub collaborative: bool,

    pub owner: SpotifyPlaylistOwner,

    /// The version of the playlist this reading came from, which removals are
    /// resolved against.
    pub snapshot_id: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct SpotifyPlaylistOwner {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// One entry in a playlist.
#[derive(Deserialize)]
pub struct SpotifyPlaylistItem {
    /// Absent when Spotify cannot resolve what was added.
    #[serde(default)]
    pub track: Option<SpotifyPlaylistItemTrack>,

    /// A file on the user's own machine rather than anything in Spotify's
    /// catalogue.
    #[serde(default)]
    pub is_local: bool,
}

/// Deliberately not [`SpotifyTrack`]: a playlist entry may be a podcast episode
/// or an unavailable track, neither of which carries everything a saved track
/// does.
#[allow(dead_code)]
#[derive(Deserialize)]
pub struct SpotifyPlaylistItemTrack {
    #[serde(default)]
    pub id: Option<String>,

    #[serde(default)]
    pub name: Option<String>,

    pub uri: String,
}

/// The occurrences of one item to remove from a playlist.
#[derive(Debug, PartialEq, Serialize)]
pub struct SpotifyPlaylistItemRemoval {
    pub uri: String,

    /// Which copies to remove. Without this Spotify removes every copy of the
    /// URI, which is the opposite of keeping one.
    pub positions: Vec<usize>,
}
