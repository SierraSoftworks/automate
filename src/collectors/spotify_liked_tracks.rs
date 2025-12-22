use std::borrow::Cow;

use crate::{collectors::IncrementalCollector, prelude::*};

pub struct SpotifyLikedTracksCollector {
    account_id: String,
    access_token: OAuth2RefreshToken,
}

impl SpotifyLikedTracksCollector {
    pub fn new(account_id: String, access_token: OAuth2RefreshToken) -> Self {
        SpotifyLikedTracksCollector {
            account_id,
            access_token,
        }
    }
}

#[async_trait::async_trait]
impl Collector for SpotifyLikedTracksCollector {
    type Item = crate::publishers::spotify::SpotifySavedTrack; // Placeholder type

    async fn list(
        &self,
        _services: &(impl crate::services::Services + Send + Sync + 'static),
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        self.fetch(_services).await
    }
}

impl IncrementalCollector for SpotifyLikedTracksCollector {
    type Watermark = chrono::DateTime<chrono::Utc>;

    fn kind(&self) -> &'static str {
        "spotify/tracks"
    }

    fn key(&self) -> Cow<'static, str> {
        Cow::Owned(self.account_id.clone())
    }

    #[instrument(
        "collectors.spotify_liked_tracks.fetch_since",
        skip(self, _services),
        err(Display)
    )]
    async fn fetch_since(
        &self,
        watermark: Option<Self::Watermark>,
        _services: &impl crate::services::Services,
    ) -> Result<(Vec<Self::Item>, Self::Watermark), human_errors::Error> {
        let client = crate::publishers::spotify::SpotifyClient::new(self.access_token.clone());

        let since = watermark.unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from(std::time::UNIX_EPOCH));

        let tracks = client.get_saved_tracks(since).await?;
        let new_watermark = tracks.iter()
            .map(|t| t.added_at)
            .max()
            .unwrap_or(since);

        Ok((tracks, new_watermark))
    }
}