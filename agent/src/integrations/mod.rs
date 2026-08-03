//! Integrations: external services which must be connected before the jobs that
//! depend on them can run.
//!
//! Registration mirrors [`crate::job`]: an implementation submits itself to an
//! [`inventory`] registry, and the generic setup routes in
//! [`crate::web::integrations`] discover it from there. That keeps the wizard,
//! the admin listing and the CSRF handling in one place no matter how many
//! services we grow.

use std::collections::BTreeMap;
use std::collections::HashMap;

pub use automate_api::{Connection, IntegrationInfo};

use crate::config::Config;
use crate::prelude::*;
use crate::services::AppServices;

pub mod github_app;
mod oauth2;

/// Where to send the visitor to begin setup, and the CSRF state to remember.
pub struct SetupRedirect {
    pub url: String,
    pub state: String,
}

/// What to tell the visitor once setup finishes.
pub struct SetupComplete {
    pub heading: String,
    pub message: String,
}

/// Everything an integration needs from the request which triggered it.
///
/// [`Integration::instances`] and [`Integration::acl`] take a bare [`Config`]
/// instead, because they are synchronous, infallible, and answerable from
/// configuration alone — they are consulted while *routing* a request, before
/// there is anything to give them a context for.
pub struct IntegrationContext<'a> {
    pub services: &'a AppServices,
    /// The public base URL, without a trailing slash.
    pub base_url: &'a str,
}

impl IntegrationContext<'_> {
    /// The absolute URL a provider should redirect back to, for integrations
    /// whose protocol requires the redirect URI to be sent with the request.
    pub fn callback_url(&self, integration: &dyn Integration, id: &str) -> String {
        format!("{}{}", self.base_url, integration.callback_path(id))
    }
}

#[async_trait::async_trait]
pub trait Integration: Send + Sync {
    /// Every integration of this kind configured on this instance.
    ///
    /// Most yield zero or one, but a family such as OAuth2 yields one entry per
    /// configured provider, which is what lets a single registration serve them
    /// all.
    fn instances(&self, config: &Config) -> Vec<IntegrationInfo>;

    /// Who may run this integration's setup wizard. `None` means admin-gated.
    fn acl(&self, config: &Config, id: &str) -> Option<Filter>;

    /// The path the provider redirects back to.
    ///
    /// Defaults to the generic route. OAuth2 providers override it because their
    /// redirect URI is registered with the provider and cannot be moved without
    /// reconfiguring the provider's application.
    fn callback_path(&self, id: &str) -> String {
        format!("/integrations/{id}/setup/callback")
    }

    async fn begin_setup(
        &self,
        id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<SetupRedirect, human_errors::Error>;

    async fn complete_setup(
        &self,
        id: &str,
        ctx: IntegrationContext<'_>,
        query: &HashMap<String, String>,
    ) -> Result<SetupComplete, human_errors::Error>;

    /// The accounts currently connected to this integration.
    async fn connections(
        &self,
        id: &str,
        ctx: IntegrationContext<'_>,
    ) -> Result<Vec<Connection>, human_errors::Error>;

    /// Severs a connection previously reported by [`Integration::connections`].
    ///
    /// Defaults to reporting that the integration cannot be disconnected here,
    /// so an implementation only opts in when it can do so meaningfully.
    async fn disconnect(
        &self,
        id: &str,
        _connection: &str,
        _ctx: IntegrationContext<'_>,
    ) -> Result<(), human_errors::Error> {
        Err(human_errors::user(
            format!("The '{id}' integration cannot be disconnected from Automate."),
            &["Remove the connection from the provider's own settings instead."],
        ))
    }
}

/// The path the transient CSRF state cookie is scoped to: the directory holding
/// the callback, so the cookie rides the provider's redirect back but is not
/// offered to the rest of the site.
pub fn state_cookie_path(integration: &dyn Integration, id: &str) -> String {
    let callback = integration.callback_path(id);
    match callback.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => callback[..index].to_string(),
    }
}

/// A registration entry for an [`Integration`], collected via [`inventory`].
pub struct IntegrationRegistration(&'static dyn Integration);

impl IntegrationRegistration {
    pub const fn new<T: Integration>(integration: &'static T) -> Self {
        Self(integration)
    }

    pub fn integration(&self) -> &'static dyn Integration {
        self.0
    }
}

inventory::collect!(IntegrationRegistration);

/// Registers an [`Integration`] so the generic setup routes pick it up.
#[macro_export]
macro_rules! register_integration {
    ($integration:expr) => {
        inventory::submit! { $crate::integrations::IntegrationRegistration::new(&$integration) }
    };
}

/// The integrations configured on this instance, resolved once at start-up.
///
/// Resolving eagerly does two things. It turns a duplicate id — an
/// `[oauth2.github]` provider alongside a GitHub App, say — into a start-up
/// failure rather than a route that silently resolves to whichever registration
/// the linker happened to emit first. And it means serving a request is a map
/// lookup instead of asking every registration to re-enumerate itself.
pub struct Registry {
    /// Ordered so the listing is stable across restarts.
    integrations: BTreeMap<String, (&'static dyn Integration, IntegrationInfo)>,
}

impl Registry {
    pub fn new(config: &Config) -> Result<Self, human_errors::Error> {
        let mut integrations: BTreeMap<String, (&'static dyn Integration, IntegrationInfo)> =
            BTreeMap::new();

        for registration in inventory::iter::<IntegrationRegistration> {
            let integration = registration.integration();

            for info in integration.instances(config) {
                if integrations.contains_key(&info.id) {
                    return Err(human_errors::user(
                        format!(
                            "Multiple integrations are configured with the id '{}'.",
                            info.id
                        ),
                        &[
                            "Integration ids share a namespace with the OAuth2 provider keys, so rename one of them.",
                            "For example, an `[oauth2.github]` provider collides with the GitHub App.",
                        ],
                    ));
                }

                integrations.insert(info.id.clone(), (integration, info));
            }
        }

        Ok(Self { integrations })
    }

    /// Every integration configured here, for the admin listing.
    pub fn list(&self) -> Vec<IntegrationInfo> {
        self.integrations
            .values()
            .map(|(_, info)| info.clone())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<(&'static dyn Integration, &IntegrationInfo)> {
        self.integrations
            .get(id)
            .map(|(integration, info)| (*integration, info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Generic;
    struct Custom;

    macro_rules! stub {
        ($name:ident, $path:expr) => {
            #[async_trait::async_trait]
            impl Integration for $name {
                fn instances(&self, _: &Config) -> Vec<IntegrationInfo> {
                    vec![]
                }
                fn acl(&self, _: &Config, _: &str) -> Option<Filter> {
                    None
                }
                fn callback_path(&self, id: &str) -> String {
                    format!($path, id = id)
                }
                async fn begin_setup(
                    &self,
                    _: &str,
                    _: IntegrationContext<'_>,
                ) -> Result<SetupRedirect, human_errors::Error> {
                    unimplemented!()
                }
                async fn complete_setup(
                    &self,
                    _: &str,
                    _: IntegrationContext<'_>,
                    _: &HashMap<String, String>,
                ) -> Result<SetupComplete, human_errors::Error> {
                    unimplemented!()
                }
                async fn connections(
                    &self,
                    _: &str,
                    _: IntegrationContext<'_>,
                ) -> Result<Vec<Connection>, human_errors::Error> {
                    unimplemented!()
                }
            }
        };
    }

    stub!(Generic, "/integrations/{id}/setup/callback");
    stub!(Custom, "/oauth/{id}/callback");

    #[test]
    fn the_state_cookie_is_scoped_to_the_callback_directory() {
        assert_eq!(
            state_cookie_path(&Generic, "github"),
            "/integrations/github/setup"
        );
        assert_eq!(state_cookie_path(&Custom, "spotify"), "/oauth/spotify");
    }

    #[tokio::test]
    async fn an_integration_cannot_be_disconnected_unless_it_opts_in() {
        let services = crate::services::ServicesContainer::new_mock()
            .await
            .unwrap();
        let err = Generic
            .disconnect(
                "github",
                "1",
                IntegrationContext {
                    services: &services,
                    base_url: "https://example.com",
                },
            )
            .await
            .expect_err("the default implementation should refuse");

        assert!(
            err.is(human_errors::Kind::User),
            "refusing is the user's problem to fix, not a bug: {err}"
        );
    }

    /// A configuration with a single OAuth2 provider, which is enough to prove
    /// the registry resolves ids through the inventory rather than a hard-coded
    /// list.
    fn config_with_spotify() -> Config {
        let mut config = Config::default();
        config.oauth2.insert(
            "spotify".to_string(),
            toml::from_str(
                r#"
                name = "Spotify"
                client_id = "id"
                client_secret = "secret"
                auth_url = "https://example.com/authorize"
                token_url = "https://example.com/token"
                "#,
            )
            .expect("the sample provider should parse"),
        );
        config
    }

    #[test]
    fn every_registered_integration_is_discoverable_by_id() {
        let registry = Registry::new(&config_with_spotify()).unwrap();

        assert_eq!(
            registry.list(),
            vec![IntegrationInfo {
                id: "spotify".to_string(),
                name: "Spotify".to_string(),
            }]
        );
        assert!(registry.get("spotify").is_some());
        assert!(registry.get("github").is_none());
    }

    #[test]
    fn a_configured_github_app_is_discoverable() {
        let mut config = config_with_spotify();
        config.connections.github.app = Some(
            toml::from_str(
                r#"
                app_id = "123456"
                slug = "my-automate"
                private_key = "-----BEGIN RSA PRIVATE KEY-----\nnot-a-real-key\n-----END RSA PRIVATE KEY-----"
                "#,
            )
            .expect("the sample app should parse"),
        );

        let registry = Registry::new(&config).unwrap();

        assert_eq!(
            registry
                .list()
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec!["github", "spotify"],
            "the listing is ordered so it does not shuffle between restarts"
        );
        assert!(registry.get("github").is_some());
    }

    /// An `[oauth2.github]` provider claims the same id as the GitHub App. Left
    /// unchecked the route would resolve to whichever registration the linker
    /// emitted first, so this must be refused outright.
    #[test]
    fn colliding_integration_ids_are_refused() {
        let mut config = Config::default();
        config.oauth2.insert(
            "github".to_string(),
            toml::from_str(
                r#"
                name = "GitHub"
                client_id = "id"
                client_secret = "secret"
                auth_url = "https://github.com/login/oauth/authorize"
                token_url = "https://github.com/login/oauth/access_token"
                "#,
            )
            .unwrap(),
        );
        config.connections.github.app = Some(
            toml::from_str(
                r#"
                app_id = "123456"
                slug = "my-automate"
                private_key = "-----BEGIN RSA PRIVATE KEY-----\nnot-a-real-key\n-----END RSA PRIVATE KEY-----"
                "#,
            )
            .unwrap(),
        );

        let err = match Registry::new(&config) {
            Ok(_) => panic!("the collision should be refused"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("github"),
            "the error should name the colliding id: {err}"
        );
    }
}
