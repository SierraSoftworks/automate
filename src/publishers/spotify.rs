use std::borrow::Cow;

use crate::prelude::*;

pub struct SpotifyClient {
    pub api_endpoint: String,
    refresh_token: OAuth2RefreshToken,
    client: reqwest::Client,
}

impl SpotifyClient {
    pub fn new(refresh_token: OAuth2RefreshToken) -> Self {
        SpotifyClient {
            api_endpoint: "https://api.spotify.com/v1".to_string(),
            refresh_token,

            client: reqwest::Client::new(),
        }
    }

    pub async fn get_current_user(&self) -> Result<SpotifyUser, human_errors::Error> {
        let user: SpotifyUser = self.call_spotify(
            reqwest::Method::GET,
            "me",
            None::<()>,
        ).await?;

        Ok(user)
    }

    pub async fn get_saved_tracks(&self, since: chrono::DateTime<chrono::Utc>) -> Result<Vec<SpotifySavedTrack>, human_errors::Error> {
        let tracks = self.call_spotify_paginated(
            reqwest::Method::GET,
            "me/tracks",
            None::<()>,
            |item: &SpotifySavedTrack| item.added_at > since,
        ).await?;

        Ok(tracks)
    }

    pub async fn get_playlists(&self) -> Result<Vec<SpotifyPlaylist>, human_errors::Error> {
        let playlists = self.call_spotify_paginated(
            reqwest::Method::GET,
            "me/playlists",
            None::<()>,
            |_| true
        ).await?;

        Ok(playlists)
    }

    pub async fn create_playlist(&self, name: impl ToString, public: bool, collaborative: bool, description: Option<String>) -> Result<SpotifyPlaylist, human_errors::Error> {
        let user = self.get_current_user().await?;

        let playlist: SpotifyPlaylist = self.call_spotify(
            reqwest::Method::POST,
            format!("users/{}/playlists", user.id),
            Some(serde_json::json!({
                "name": name.to_string(),
                "public": public,
                "collaborative": collaborative,
                "description": description,
            })),
        ).await?;

        Ok(playlist)
    }

    pub async fn add_tracks_to_playlist(&self, playlist_id: impl ToString, track_uris: Vec<String>) -> Result<(), human_errors::Error> {
        let _: () = self.call_spotify(
            reqwest::Method::POST,
            format!("playlists/{}/tracks", playlist_id.to_string()),
            Some(serde_json::json!({
                "uris": track_uris,
            })),
        ).await?;

        Ok(())
    }

    async fn call_spotify_paginated<T: DeserializeOwned, W: Fn(&T) -> bool>(&self, method: reqwest::Method, path: impl Into<Cow<'_, str>>, json: Option<impl serde::Serialize>, filter: W) -> Result<Vec<T>, human_errors::Error> {
        let mut results = Vec::new();
        let mut url = path.into().to_string();

        loop {
            let resp: PaginatedResponse<T> = self.call_spotify(method.clone(), url, json.as_ref()).await?;

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

    async fn call_spotify<T: DeserializeOwned>(&self, method: reqwest::Method, path: impl Into<Cow<'_, str>>, json: Option<impl Serialize>) -> Result<T, human_errors::Error> {
        let access_token = self.refresh_token.access_token();

        let path = path.into();
        let url = if path.starts_with(&self.api_endpoint) {
            path.into_owned()
        } else {
            format!("{}/{}", self.api_endpoint, path)
        };

        let req = self.client.request(method, url)
            .bearer_auth(access_token);

        let req = if let Some(json) = json {
            req.json(&json)
        } else {
            req
        };

        let req = req.build().map_err_as_system(&[
            "Report this issue to the development team on GitHub."
        ])?;

        let resp = self.client.execute(req).await.map_err_as_user(&[
            "Make sure that your internet connection is working."
        ])?.error_for_status().wrap_err_as_user("Failed to call Spotify's API", &[
            "Ensure that your internet connection is working.",
            "Check that Spotify's service is operational.",
        ])?;

        resp.json().await.map_err_as_user(&[
            "Ensure that your internet connection is working.",
            "Check that Spotify's service is operational.",
        ])
    }

    pub async fn renew_access_token(token: &OAuth2RefreshToken, services: &(impl Services + Send + Sync + 'static)) -> Result<OAuth2RefreshToken, human_errors::Error> {
        let config = services.config().get_oauth2("spotify")?;
        config.get_access_token(token).await
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
    pub public: bool,
    pub collaborative: bool,
}