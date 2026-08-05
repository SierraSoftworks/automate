use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::jobs::*;
use crate::prelude::*;
use crate::web::*;

#[derive(Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub connections: ConnectionConfigs,
    #[serde(default)]
    pub oauth2: HashMap<String, OAuth2Config>,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub workflows: WorkflowConfigs,
}

impl Config {
    /// Loads environment variables from a .env file if it exists.
    /// Variables from the .env file will override process-level environment variables.
    pub fn load_env_file(path: impl AsRef<Path>) -> Result<(), human_errors::Error> {
        let path = path.as_ref();

        if !path.exists() {
            // It's okay if the file doesn't exist
            return Ok(());
        }

        dotenvy::from_path_override(path).wrap_user_err(
            format!(
                "We could not load your environment file '{}'.",
                path.display()
            ),
            &[
                "Ensure the file is in the correct .env format (KEY=value).",
                "Check that you have the necessary permissions to read the file.",
            ],
        )?;

        Ok(())
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, human_errors::Error> {
        let path = path.into();
        let contents = std::fs::read_to_string(&path).wrap_user_err(
            format!("We could not read your config file '{}'.", path.display()),
            &[
                "Ensure the file exists and is readable.",
                "Check that you have the necessary permissions to read the file.",
            ],
        )?;

        // Interpolate environment variables before parsing TOML
        let contents = crate::parsers::interpolate(&contents, |expr| {
            let expr = expr.trim();
            if let Some(var_name) = expr.strip_prefix("env.") {
                Ok(std::env::var(var_name).unwrap_or_else(|_| format!("${{{{ {} }}}}", expr)))
            } else {
                Err(human_errors::user(
                    format!("Unknown interpolation expression: '{}'", expr),
                    &[
                        "Currently, only 'env.VARIABLE_NAME' expressions are supported.",
                        "Use '\\${{ ... }}' to escape literal text that looks like an expression.",
                    ],
                ))
            }
        })?;

        let config: Config = toml::from_str(&contents).wrap_user_err(
            "Your configuration file could not be loaded.",
            &[
                "Ensure that the file is valid TOML.",
                "Make sure that you are using the correct configuration file format.",
            ],
        )?;
        Ok(config)
    }

    pub fn get_oauth2(&self, kind: &str) -> Result<OAuth2Config, human_errors::Error> {
        self.oauth2.get(kind).cloned().ok_or_else(|| {
            human_errors::user(
                format!("OAuth configuration for kind '{}' not found.", kind),
                &[
                    "Ensure that the OAuth configuration is present in your config file.",
                    "Check that the kind value is correct.",
                ],
            )
        })
    }
}

#[derive(Default, Clone, Deserialize)]
pub struct ConnectionConfigs {
    /// The Todoist credential an installation used to share.
    ///
    /// Superseded by per-account connections. Kept only so that an existing
    /// configuration file still loads and can be imported once on start-up; see
    /// [`crate::connections::import_configured_credentials`].
    #[serde(default)]
    pub todoist: LegacyApiKey,

    #[serde(default)]
    pub github: GitHubConfig,

    #[serde(default)]
    pub ynab: YnabConfig,

    #[serde(default)]
    pub alphavantage: AlphaVantageConfig,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    #[serde(default = "default_listen_address")]
    pub address: String,

    /// Where the SQLite database lives.
    ///
    /// The encryption key for stored credentials is kept in a file alongside it
    /// unless one is configured explicitly, so moving this moves both.
    #[serde(default = "default_database_path")]
    pub database: String,

    /// Identity and access configuration.
    #[serde(default)]
    pub auth: AuthConfig,

    /// The pre-multi-tenant admin section, retained so existing configuration
    /// files keep working. Prefer `[web.auth]`; see [`WebConfig::user_acl`] for
    /// how the two are reconciled.
    #[serde(default)]
    pub admin: AdminConfig,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Whether to trust reverse-proxy forwarding headers (`X-Forwarded-Proto`,
    /// `X-Forwarded-Host`) when determining the request's scheme and host. Only
    /// enable this when the service sits behind a trusted proxy that sets these
    /// headers, otherwise a client could spoof them.
    #[serde(default)]
    pub trust_proxy: bool,
}

impl Default for WebConfig {
    /// Written out rather than derived, because a derived `Default` would give
    /// empty strings for the fields that carry a `#[serde(default = "...")]` —
    /// serde's defaults apply only when deserialising, so the two paths would
    /// otherwise disagree about what an unconfigured agent listens on and where
    /// it keeps its database.
    fn default() -> Self {
        Self {
            address: default_listen_address(),
            database: default_database_path(),
            auth: AuthConfig::default(),
            admin: AdminConfig::default(),
            base_url: None,
            trust_proxy: false,
        }
    }
}

#[allow(dead_code)]
impl WebConfig {
    /// The filter deciding who may sign in at all.
    ///
    /// Falls back to the legacy `[web.admin] acl`. Under that configuration
    /// there was only one gate — passing it granted full access — so it governs
    /// both signing in and administrator status, which preserves the behaviour
    /// an existing installation already has.
    pub fn user_acl(&self) -> &Filter {
        self.auth.user_acl.as_ref().unwrap_or(&self.admin.acl)
    }

    /// The filter deciding who is an administrator.
    pub fn admin_acl(&self) -> &Filter {
        self.auth.admin_acl.as_ref().unwrap_or(&self.admin.acl)
    }

    /// The identity provider to authenticate against, if any.
    pub fn oidc(&self) -> Option<&OidcConfig> {
        self.auth.oidc.as_ref().or(self.admin.oidc.as_ref())
    }
}

/// Identity, access control and credential protection.
///
/// Like the other configuration types, this describes the shape of the file
/// rather than a set of call sites, so not every field has a reader in every
/// build.
#[allow(dead_code)]
#[derive(Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// Whether several people may sign in and each hold their own workflows and
    /// connections.
    ///
    /// Off by default, so that upgrading an existing installation changes
    /// nothing until the operator opts in.
    #[serde(default)]
    pub multi_tenant: bool,

    /// The account that owns everything when no identity provider is configured.
    ///
    /// Naming this after the username you will eventually sign in as means that
    /// adopting an identity provider later leaves your existing workflows where
    /// they are, rather than needing them moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_user: Option<String>,

    /// Who may sign in. Defaults to the legacy `[web.admin] acl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_acl: Option<Filter>,

    /// Who may administer the installation, including impersonating another
    /// user. Defaults to the legacy `[web.admin] acl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_acl: Option<Filter>,

    /// The key used to encrypt stored credentials, as base64 or hexadecimal.
    ///
    /// When absent, a key is generated into a file beside the database on first
    /// run. Set this to keep the key in your own secret management instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_key: Option<String>,

    /// Keys previously used to encrypt credentials.
    ///
    /// Values are only ever encrypted with the current key, but are decrypted
    /// with whichever key sealed them, so a rotated key stays listed here until
    /// every record that used it has been rewritten.
    #[serde(default)]
    pub previous_secret_keys: Vec<String>,

    /// The identity provider to authenticate against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc: Option<OidcConfig>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminConfig {
    /// Filter expression that must evaluate to true for a request to be granted
    /// access to the admin endpoints. When OIDC is configured, validated token
    /// claims are exposed to the filter under the `claims.` prefix. Defaults to
    /// denying every request so that the admin area is closed unless explicitly
    /// opened up.
    #[serde(default = "default_admin_acl")]
    pub acl: Filter,

    /// Optional OIDC configuration. When present, requests to the admin API must
    /// carry a valid `Authorization: Bearer` ID token issued by the configured
    /// provider. The admin SPA runs the Authorization Code request in a popup and
    /// the agent performs the confidential code exchange (and refreshes) with its
    /// `client_secret`; the resulting ID token is held by the SPA and presented as
    /// a bearer, never as a cookie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc: Option<OidcConfig>,
}

impl Default for AdminConfig {
    fn default() -> Self {
        // Deny by default: when the `[web.admin]` section is omitted entirely we
        // must close the admin area rather than fall back to the permissive
        // `Filter::default()` (which evaluates to `true`).
        Self {
            acl: default_admin_acl(),
            oidc: None,
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct OidcConfig {
    /// The base URL of the OIDC provider (its issuer), used to discover the
    /// provider's endpoints via `{endpoint}/.well-known/openid-configuration`.
    pub endpoint: String,

    /// The OAuth2 client ID registered with the provider. This is also used as
    /// the expected audience (`aud`) when validating ID tokens.
    pub client_id: String,

    /// The OAuth2 client secret registered with the provider.
    pub client_secret: String,

    /// The scopes to request when authenticating. `openid` is always included.
    /// Include `offline_access` (or the provider's equivalent) so the provider
    /// issues a refresh token — without one the SPA cannot renew an expired
    /// session transparently and must prompt for an interactive sign-in instead.
    #[serde(default)]
    pub scopes: Vec<String>,

    /// The claim identifying the account, which becomes the key everything the
    /// user owns is stored under.
    ///
    /// Defaults to `preferred_username`, falling back to `sub` where the
    /// provider does not supply one. Point this at a different claim if yours
    /// puts a stable, unique name elsewhere — but choose one that does not
    /// change, because renaming an account is a deliberate migration rather
    /// than something that happens on its own.
    #[allow(dead_code)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username_claim: Option<String>,
}

fn default_listen_address() -> String {
    "localhost:8080".to_string()
}

fn default_database_path() -> String {
    "database.sqlite".to_string()
}

fn default_admin_acl() -> Filter {
    Filter::new("false").expect("the literal `false` filter is always valid")
}

#[derive(Clone, Deserialize, Default)]
pub struct WorkflowConfigs {
    #[serde(default)]
    pub calendars: Vec<CronJobConfig<CalendarWorkflow>>,
    #[serde(default)]
    pub github_notifications: Vec<CronJobConfig<GitHubNotificationsWorkflow>>,
    #[serde(default)]
    pub github_notifications_cleanup: CronJobConfig<GitHubNotificationsCleanupWorkflow>,
    #[serde(default)]
    pub github_releases: Vec<CronJobConfig<GitHubReleasesWorkflow>>,
    #[serde(default)]
    pub rss: Vec<CronJobConfig<RssWorkflow>>,
    #[serde(default)]
    pub youtube: Vec<CronJobConfig<YouTubeWorkflow>>,
    #[serde(default)]
    pub xkcd: Vec<CronJobConfig<XkcdWorkflow>>,
    #[serde(default)]
    pub ynab_stocks: Vec<CronJobConfig<YnabStocksWorkflow>>,
}

#[derive(Default, Clone, Deserialize)]
pub struct GitHubConfig {
    /// A classic personal access token. This cannot be replaced by the App: the
    /// notifications API accepts classic PATs only, and the releases workflow
    /// reads repositories no installation of ours covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// The GitHub App used for management calls, so that writes are attributed
    /// to the App and scoped to the repositories each installation grants.
    #[serde(default)]
    pub app: Option<GitHubAppConfig>,
}

#[derive(Clone, Deserialize)]
pub struct GitHubAppConfig {
    /// The App's numeric ID, from its settings page.
    pub app_id: String,

    /// The App's PEM-encoded private key, including its BEGIN and END lines.
    pub private_key: String,

    /// The App's URL slug, used to build the link the install wizard sends
    /// people to.
    pub slug: String,

    /// The base URL of the GitHub API the App's management calls are addressed
    /// to, defaulting to `https://api.github.com`.
    ///
    /// A GitHub Enterprise Server instance serves its API from its own host, at
    /// `https://<hostname>/api/v3`, so an App registered there is unreachable
    /// without this. The paths below it are the same ones github.com serves.
    #[serde(default)]
    pub api_url: Option<String>,

    /// Who may use the install wizard. Evaluated exactly like the OAuth2
    /// providers' `acl`, and admin-gated when omitted.
    #[serde(default)]
    pub acl: Option<Filter>,
}

#[derive(Default, Clone, Deserialize)]
pub struct YnabConfig {
    /// The YNAB Personal Access Token used to authenticate with the YNAB API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// A credential that used to live in the configuration file.
#[derive(Default, Clone, Deserialize)]
pub struct LegacyApiKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Default, Clone, Deserialize)]
pub struct AlphaVantageConfig {
    /// The AlphaVantage API key used to fetch stock quotes and exchange rates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).expect("the configuration should parse")
    }

    #[test]
    fn a_legacy_admin_section_still_governs_access() {
        // Before multi-tenancy there was one gate, and passing it granted full
        // access. An existing configuration must keep behaving that way rather
        // than silently locking its operator out.
        let config = parse(
            r#"
            [web.admin]
            acl = 'client_ip in ["127.0.0.1"]'

            [web.admin.oidc]
            endpoint = "https://id.example.com"
            client_id = "automate"
            client_secret = "shh"
        "#,
        );

        assert_eq!(
            config.web.user_acl().to_string(),
            config.web.admin.acl.to_string()
        );
        assert_eq!(
            config.web.admin_acl().to_string(),
            config.web.admin.acl.to_string()
        );
        assert_eq!(
            config.web.oidc().map(|o| o.endpoint.as_str()),
            Some("https://id.example.com")
        );
    }

    #[test]
    fn the_auth_section_takes_precedence_over_the_legacy_one() {
        let config = parse(
            r#"
            [web.admin]
            acl = 'false'

            [web.auth]
            user_acl = 'true'
            admin_acl = 'claims.email == "admin@example.com"'

            [web.auth.oidc]
            endpoint = "https://new.example.com"
            client_id = "automate"
            client_secret = "shh"
        "#,
        );

        assert_eq!(config.web.user_acl().to_string(), "true");
        assert_eq!(
            config.web.admin_acl().to_string(),
            r#"claims.email == "admin@example.com""#
        );
        assert_eq!(
            config.web.oidc().map(|o| o.endpoint.as_str()),
            Some("https://new.example.com")
        );
    }

    #[test]
    fn access_is_denied_by_default_when_nothing_is_configured() {
        let config = Config::default();

        assert_eq!(config.web.user_acl().to_string(), "false");
        assert_eq!(config.web.admin_acl().to_string(), "false");
        assert!(config.web.oidc().is_none());
        assert!(!config.web.auth.multi_tenant);
    }

    #[test]
    fn the_database_path_defaults_to_the_working_directory() {
        assert_eq!(Config::default().web.database, "database.sqlite");
        assert_eq!(
            parse("[web]\ndatabase = \"/var/lib/automate/db.sqlite\"")
                .web
                .database,
            "/var/lib/automate/db.sqlite"
        );
    }

    #[test]
    fn a_misplaced_web_key_is_reported_rather_than_ignored() {
        // `admin_acl` belongs under [web.auth]; at the [web] level it used to be
        // dropped silently, leaving the operator with the deny-by-default ACL
        // and no indication why.
        let err = match toml::from_str::<Config>(
            r#"
            [web]
            admin_acl = 'client_ip in ["127.0.0.1"]'
        "#,
        ) {
            Err(err) => err,
            Ok(_) => panic!("an unrecognised key under [web] should be refused"),
        };

        assert!(err.to_string().contains("admin_acl"), "{err}");
    }

    #[test]
    fn the_documented_example_configuration_is_valid() {
        // `deny_unknown_fields` makes the example file a real test of the
        // schema: any key documented there that the agent does not understand
        // now fails here rather than being silently ignored at runtime.
        let example = include_str!("../../config.example.toml");

        if let Err(err) = toml::from_str::<Config>(example) {
            panic!("config.example.toml does not match the configuration schema: {err}");
        }
    }

    #[test]
    fn the_username_claim_is_optional_and_unset_by_default() {
        let config = parse(
            r#"
            [web.auth.oidc]
            endpoint = "https://id.example.com"
            client_id = "automate"
            client_secret = "shh"
        "#,
        );

        assert_eq!(config.web.oidc().unwrap().username_claim, None);
    }

    #[test]
    fn test_load_env_file_with_valid_file() {
        let temp_dir = std::env::temp_dir();
        let env_file = temp_dir.join(format!("test_env_{}.env", uuid::Uuid::new_v4()));

        // Create a test .env file
        let mut file = std::fs::File::create(&env_file).unwrap();
        writeln!(file, "TEST_VAR_1=value1").unwrap();
        writeln!(file, "TEST_VAR_2=value2").unwrap();
        writeln!(file, "# Comment line").unwrap();
        writeln!(file, "TEST_VAR_3=\"value with spaces\"").unwrap();
        drop(file);

        // Load the env file
        Config::load_env_file(&env_file).unwrap();

        // Verify the variables were loaded
        assert_eq!(std::env::var("TEST_VAR_1").unwrap(), "value1");
        assert_eq!(std::env::var("TEST_VAR_2").unwrap(), "value2");
        assert_eq!(std::env::var("TEST_VAR_3").unwrap(), "value with spaces");

        // Cleanup
        std::fs::remove_file(&env_file).ok();
    }

    #[test]
    fn test_load_env_file_nonexistent_file() {
        let temp_dir = std::env::temp_dir();
        let env_file = temp_dir.join("nonexistent.env");

        // Should not error when file doesn't exist
        let result = Config::load_env_file(&env_file);
        assert!(result.is_ok());
    }

    #[test]
    fn test_env_interpolation_in_config() {
        let temp_dir = std::env::temp_dir();
        let config_file = temp_dir.join(format!("test_config_{}.toml", uuid::Uuid::new_v4()));
        let env_file = temp_dir.join(format!("test_env_{}.env", uuid::Uuid::new_v4()));

        // Create a test .env file
        let mut file = std::fs::File::create(&env_file).unwrap();
        writeln!(file, "TEST_API_KEY=secret123").unwrap();
        drop(file);

        // Create a test config file with interpolation
        let mut file = std::fs::File::create(&config_file).unwrap();
        writeln!(file, "[connections.github]").unwrap();
        writeln!(file, "api_key = \"${{{{ env.TEST_API_KEY }}}}\"").unwrap();
        drop(file);

        // Load env file and config
        Config::load_env_file(&env_file).unwrap();
        let config = Config::load(&config_file).unwrap();

        // Verify interpolation worked
        assert_eq!(
            config.connections.github.api_key.as_deref(),
            Some("secret123")
        );

        // Cleanup
        std::fs::remove_file(&config_file).ok();
        std::fs::remove_file(&env_file).ok();
    }
}
