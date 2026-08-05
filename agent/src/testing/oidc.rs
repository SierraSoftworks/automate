//! An identity provider that tests can actually sign in to.
//!
//! # Why this exists
//!
//! Until now nothing could produce a token the agent would accept, so two
//! things went untested. The first is [`crate::web::helpers::oidc::validate_token`]
//! itself — every path through it that ends in "yes" was unreachable, and the
//! paths that end in "no" could only be reached with tokens so malformed they
//! never got as far as a signature check. The second is the middleware's
//! decision about *which account* a request acts for: the tenancy tests could
//! prove that `Principal::effective` partitions everything downstream, but not
//! that signing in as somebody produces the right `effective` in the first
//! place.
//!
//! # Why it is a real provider and not a shortcut
//!
//! The cheap way to reach both would be a `#[cfg(test)]` branch in the
//! validation path — accept HS256 signed with a shared secret, skip the
//! discovery and JWKS fetches, done in twenty lines. That branch is the
//! algorithm-confusion vulnerability written down deliberately, and worse, it
//! means the thing under test is not the thing that ships: the production path
//! keeps the guard, the tested path never exercises it, and nothing notices
//! when they drift. This repository removed exactly that kind of divergence
//! from the GitHub App client, where a `new_for_test` constructor signed with a
//! symmetric key while the real one signed with RSA.
//!
//! So this is a provider, not a stub. It serves a real discovery document over
//! HTTP, a real JWKS derived from a real RSA key, and mints real RS256 tokens.
//! The agent fetches, caches, verifies and rotates against it exactly as it
//! would against Entra or Auth0, and there is no branch anywhere in
//! `agent/src` that knows a test is running.

use std::sync::LazyLock;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::config::{Config, OidcConfig};
use crate::services::AppContext;

/// Where the provider publishes its signing keys.
///
/// Named in the discovery document rather than assumed by the agent, so a test
/// that broke discovery parsing would fail here rather than quietly working.
const JWKS_PATH: &str = "/jwks";

/// The path OIDC reserves for discovery. Not ours to choose, which is the point
/// — the agent appends it to the configured endpoint, and if it ever appended
/// something else these mocks would stop matching.
const DISCOVERY_PATH: &str = "/.well-known/openid-configuration";

/// The `kid` the provider advertises and stamps into every token it signs.
pub const KEY_ID: &str = "test-provider-2026";

/// The client this agent is registered as, and therefore the audience every
/// token it will accept must name.
pub const CLIENT_ID: &str = "automate-under-test";

/// How long an issued token is good for, in seconds.
///
/// Long enough that no test can lose a race against it, short enough to be
/// obviously a session rather than a permanent credential.
const TOKEN_LIFETIME_SECONDS: i64 = 3600;

/// The key material the test provider signs with.
///
/// Held as the PEM plus the public half already in JWK form, because the JWKS
/// is served on every request the agent makes and re-deriving the modulus each
/// time would be work for nothing.
struct ProviderKey {
    pem: String,
    jwk: serde_json::Value,
}

/// The provider's signing key, generated once per test process.
///
/// # Why it is generated rather than committed
///
/// The same reason [`super::GITHUB_APP_PRIVATE_KEY`] is: a private key in a
/// repository is one that eventually gets copied somewhere real, because it
/// looks like the key you are supposed to use. One that only exists for the
/// length of a test process cannot be.
///
/// # Why it is generated once
///
/// RSA key generation is slow enough to dominate a suite that otherwise runs in
/// a second or two, so this is one key per test process rather than one per
/// test, exactly as the GitHub App's is.
///
/// # Why it is not the GitHub App's key
///
/// They are the same kind of object doing opposite jobs. The App's key is the
/// agent's own credential, the thing it signs with to prove who it is; this one
/// belongs to somebody else entirely, and the agent must only ever see its
/// public half. Sharing one static between them would make "the agent verifies
/// against a key it also holds the private half of" true throughout the tests —
/// which is the precise confusion this whole module exists to avoid, and would
/// let a test that mistakenly signed an ID token with the App's key pass. The
/// cost of keeping them apart is one extra key generation per test process.
///
/// The App's key is still borrowed for the one job where being a *different*
/// key is the entire point; see [`unadvertised_key`].
static PROVIDER_KEY: LazyLock<ProviderKey> = LazyLock::new(|| {
    use base64::Engine;
    use rsa::RsaPrivateKey;
    use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
    use rsa::traits::PublicKeyParts;

    // 2048 bits because that is what identity providers issue, so this signs
    // and verifies at exactly the cost the real thing does.
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
        .expect("generate a signing key for the identity provider under test");

    let pem = key
        .to_pkcs1_pem(LineEnding::LF)
        .expect("encode the generated signing key as PEM")
        .to_string();

    // RFC 7518 §6.3.1: the modulus and exponent as big-endian octet strings,
    // base64url without padding. Derived from the key we just generated rather
    // than written down, so the JWKS cannot drift out of step with what the
    // provider actually signs with.
    let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let jwk = serde_json::json!({
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": KEY_ID,
        "n": encoder.encode(key.n().to_bytes_be()),
        "e": encoder.encode(key.e().to_bytes_be()),
    });

    ProviderKey { pem, jwk }
});

/// A real RSA key that the test provider does not advertise.
///
/// This is the GitHub App's key, borrowed rather than generated. It is just as
/// real and just as valid; what makes it the right key for the job is that it
/// is one *this installation holds* — so a token signed with it asks the
/// question that matters. Does holding one of the agent's own keys let you mint
/// a sign-in? Generating a third key to ask the same thing would cost another
/// RSA key generation in every test process and say less.
pub fn unadvertised_key() -> String {
    super::GITHUB_APP_PRIVATE_KEY.clone()
}

/// An OIDC provider, served over HTTP, that the agent can genuinely sign people
/// in against.
///
/// Holds the mock server, so it must outlive every request made through it —
/// dropping this shuts the provider down and the agent starts reporting that
/// its identity provider is unreachable, which is at least an honest failure.
pub struct TestIdentityProvider {
    server: MockServer,
}

impl TestIdentityProvider {
    /// Starts a provider and publishes its discovery document and signing keys.
    pub async fn start() -> Self {
        let server = MockServer::start().await;

        // A real discovery document, naming real endpoints on this server. The
        // agent reads `issuer` from here and requires every token to match it,
        // so this is not decoration.
        Mock::given(method("GET"))
            .and(path(DISCOVERY_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "authorization_endpoint": format!("{}/authorize", server.uri()),
                "token_endpoint": format!("{}/token", server.uri()),
                "jwks_uri": format!("{}{JWKS_PATH}", server.uri()),
            })))
            .mount(&server)
            .await;

        // Only the public half. Serving anything more would be a provider that
        // hands out its own signing key, and the agent would be right to be
        // tested against one that does not.
        Mock::given(method("GET"))
            .and(path(JWKS_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "keys": [PROVIDER_KEY.jwk] })),
            )
            .mount(&server)
            .await;

        Self { server }
    }

    /// The provider's issuer, which is also where its discovery document lives.
    pub fn issuer(&self) -> String {
        self.server.uri()
    }

    /// The configuration an operator would write to trust this provider.
    ///
    /// Ordinary configuration with nothing test-shaped about it: an endpoint, a
    /// client, a secret. Where the endpoint points is the only thing a test
    /// arranges, and that is something an operator arranges too.
    pub fn oidc_config(&self) -> OidcConfig {
        OidcConfig {
            endpoint: self.issuer(),
            client_id: CLIENT_ID.to_string(),
            client_secret: "test-client-secret".to_string(),
            scopes: vec![],
            username_claim: None,
        }
    }

    /// The claims this provider puts in an ID token for `username`.
    ///
    /// `sub` is deliberately not the username. Real providers issue an opaque
    /// subject identifier alongside a human-readable name, and keeping them
    /// distinct here means a test can tell which claim the agent actually reads
    /// to decide whose account a request belongs to.
    pub fn claims_for(&self, username: &str) -> serde_json::Value {
        let now = chrono::Utc::now().timestamp();

        serde_json::json!({
            "iss": self.issuer(),
            "aud": CLIENT_ID,
            "sub": format!("subject-id-for-{username}"),
            "preferred_username": username,
            "name": display_name(username),
            "email": format!("{username}@example.com"),
            "iat": now,
            "exp": now + TOKEN_LIFETIME_SECONDS,
        })
    }

    /// A signed ID token identifying `username`, as the browser would present
    /// it after a sign-in.
    pub fn sign_in_as(&self, username: &str) -> String {
        self.issue(self.claims_for(username))
    }

    /// Signs an arbitrary claim set with the key this provider advertises.
    ///
    /// For the cases where what is being tested is the claim set itself — an
    /// expired token, one minted for somebody else's audience — rather than who
    /// is signing in.
    pub fn issue(&self, claims: serde_json::Value) -> String {
        self.issue_with_kid(Some(KEY_ID), claims)
    }

    /// Signs a claim set with the provider's own key but labels it with `kid`.
    ///
    /// The signature is genuinely the provider's; only the label is wrong. That
    /// separates "we could not verify this" from "we could not work out what to
    /// verify it against", which are different refusals for different reasons.
    pub fn issue_with_kid(&self, kid: Option<&str>, claims: serde_json::Value) -> String {
        self.issue_signed_by(&PROVIDER_KEY.pem, kid, claims)
    }

    /// Signs a claim set with an arbitrary key and `kid`.
    ///
    /// The forgery constructor: everything a real token has except a signature
    /// the agent should be willing to trust.
    pub fn issue_signed_by(
        &self,
        pem: &str,
        kid: Option<&str>,
        claims: serde_json::Value,
    ) -> String {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = kid.map(str::to_string);

        jsonwebtoken::encode(
            &header,
            &claims,
            &jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes())
                .expect("read the signing key for the identity provider under test"),
        )
        .expect("sign an ID token")
    }

    /// How many times the agent has fetched this provider's signing keys.
    ///
    /// Counted from what the provider was actually asked for rather than from a
    /// mock's expectation, so a test can assert both that a refetch happened and
    /// that no further ones did.
    pub async fn jwks_fetches(&self) -> usize {
        self.requests_to(JWKS_PATH).await
    }

    /// How many times the agent has fetched this provider's discovery document.
    pub async fn discovery_fetches(&self) -> usize {
        self.requests_to(DISCOVERY_PATH).await
    }

    async fn requests_to(&self, wanted: &str) -> usize {
        self.server
            .received_requests()
            .await
            .expect("the mock server records the requests it receives")
            .iter()
            .filter(|request| request.url.path() == wanted)
            .count()
    }

    /// An installation that trusts this provider, keeps each person's records
    /// apart, and lets anybody who can sign in do so.
    ///
    /// The one-line form, which is what almost every test wants: what is being
    /// tested is who somebody turns out to be, not whether an ACL admits them.
    pub async fn context(&self) -> AppContext {
        self.context_with(|_| {}).await
    }

    /// As [`Self::context`], letting the caller adjust the configuration.
    ///
    /// The caller's changes are applied last, so anything set here can be
    /// overridden — including the identity provider itself, for a test about
    /// what happens without one.
    pub async fn context_with(&self, f: impl Sized + FnOnce(&mut Config)) -> AppContext {
        let oidc = self.oidc_config();

        AppContext::new_mock(move |config| {
            config.web.auth.oidc = Some(oidc);
            config.web.auth.user_acl = Some(crate::filter::Filter::new("true").unwrap());
            // Nobody administers anything unless a test asks for it. Two
            // ordinary people who cannot see one another's records is the
            // property worth holding; two views an administrator takes is a
            // weaker one, and it already has its own tests.
            config.web.auth.admin_acl = Some(crate::filter::Filter::new("false").unwrap());
            // Without this there is only one account, and everybody who signs in
            // lands in it — which is the documented upgrade behaviour, but it
            // leaves nothing to keep apart.
            config.web.auth.multi_tenant = true;

            f(config);
        })
        .await
        .expect("build a context trusting the identity provider under test")
    }
}

/// The name a provider would show for somebody, given their username.
///
/// Distinct from the username on purpose: `name` and `preferred_username` are
/// separate claims read by separate code paths, and a test cannot tell them
/// apart if they carry the same value.
fn display_name(username: &str) -> String {
    let mut characters = username.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}
