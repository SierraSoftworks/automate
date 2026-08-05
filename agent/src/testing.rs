use std::path::PathBuf;
use std::sync::LazyLock;

use crate::config::GitHubAppConfig;

pub fn get_test_file_path<P: AsRef<str>>(name: P) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name.as_ref())
}

pub fn get_test_file_contents<P: AsRef<str>>(name: P) -> String {
    let path = get_test_file_path(name);
    std::fs::read_to_string(path).unwrap_or_else(|_| panic!("Failed to read test file"))
}

pub async fn mock_services() -> Result<impl crate::services::Services, human_errors::Error> {
    crate::services::ServicesContainer::new_mock().await
}

/// The PEM private key the GitHub App under test signs its JWTs with.
///
/// # Why it is generated rather than committed
///
/// A private key checked into a repository is a private key that eventually
/// gets copied into something real — a deployment, a support ticket, a
/// screenshot — because it looks like the key you are supposed to use and
/// nothing about it says otherwise. One that only exists for the length of a
/// test process cannot be: there is nothing to copy, and it is different every
/// run. That it is generated is also why the tests can exercise
/// [`crate::services::GitHubAppClient::new`] itself, rather than a second
/// constructor standing in for it.
///
/// # Why it is generated once
///
/// RSA key generation is slow — appreciably slower than every other thing these
/// tests do put together — so a key per test would turn a suite that runs in a
/// second or two into one nobody waits for. The `LazyLock` makes it one key per
/// test process, generated the first time a test asks for an App.
///
/// 2048 bits because that is what GitHub issues, so this signs exactly as the
/// real thing does.
static GITHUB_APP_PRIVATE_KEY: LazyLock<String> = LazyLock::new(|| {
    use rsa::RsaPrivateKey;
    use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};

    RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
        .expect("generate a private key for the GitHub App under test")
        .to_pkcs1_pem(LineEnding::LF)
        .expect("encode the generated private key as PEM")
        .to_string()
});

/// A GitHub App whose management calls are addressed to `api_url`.
///
/// The key is real, so a client built from this signs and is read back exactly
/// as one built from an operator's configuration is; only where the calls land
/// differs, and that is a thing the configuration says out loud rather than
/// something the test build arranges behind the code's back.
pub fn github_app(api_url: impl Into<String>) -> GitHubAppConfig {
    GitHubAppConfig {
        app_id: "123".to_string(),
        private_key: GITHUB_APP_PRIVATE_KEY.clone(),
        slug: "my-automate".to_string(),
        api_url: Some(api_url.into()),
        acl: None,
    }
}
