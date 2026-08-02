use actix_web::{HttpRequest, HttpResponse, dev::HttpServiceFactory, web};
use oauth2::CsrfToken;

use crate::prelude::*;
use crate::services::GitHubInstallation;
use crate::web::oauth::{
    OAUTH_SETUP_STATE_COOKIE, PublicWizardOutcome, access_denied_page, admin_only_page, error_page,
    html_action_page, html_page, public_wizard_outcome, state_matches, with_cleared_state,
    wizard_state_cookie,
};

/// The kv partition recording which accounts have installed the App, keyed on
/// the account's login.
pub const INSTALLATIONS_PARTITION: &str = "github/installations";

const WIZARD_PATH: &str = "/github/install";

pub fn configure<S: Services + Send + Sync + 'static>() -> impl HttpServiceFactory {
    web::scope(WIZARD_PATH)
        .route("/", web::get().to(install_home::<S>))
        .route("/authorize", web::get().to(install_authorize::<S>))
        .route("/callback", web::get().to(install_callback::<S>))
}

/// Records an installation so the admin area can list which accounts are
/// connected.
///
/// Keyed on the account rather than the installation id, because reinstalling
/// an App issues a fresh id for the same account and we want the newer one to
/// replace the old rather than accumulate beside it.
pub async fn record_installation(
    installation: &GitHubInstallation,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<(), human_errors::Error> {
    services
        .kv()
        .set(
            INSTALLATIONS_PARTITION,
            installation.account.clone(),
            installation.clone(),
        )
        .await
}

pub async fn forget_installation(
    account: &str,
    services: &(impl Services + Send + Sync + 'static),
) -> Result<(), human_errors::Error> {
    services
        .kv()
        .remove(INSTALLATIONS_PARTITION, account.to_string())
        .await
}

async fn install_home<S: Services + Send + Sync + 'static>(
    services: web::Data<S>,
    req: HttpRequest,
) -> HttpResponse {
    let config = services.config();
    let Some(app) = config.connections.github.app.as_ref() else {
        return error_page(
            404,
            "Not Found",
            "No GitHub App is configured on this Automate instance.",
        );
    };

    match public_wizard_outcome(services.as_ref(), &req, app.acl.as_ref()) {
        PublicWizardOutcome::AdminOnly => admin_only_page(),
        PublicWizardOutcome::Denied => access_denied_page(),
        PublicWizardOutcome::Allowed => html_action_page(
            "GitHub | Automate",
            "Install the GitHub App",
            "Choose the user account or organization whose repositories Automate should watch. You can install it on as many accounts as you like, and change which repositories it covers at any time.",
            &format!("{WIZARD_PATH}/authorize"),
            "Install",
        ),
    }
}

async fn install_authorize<S: Services + Send + Sync + 'static>(
    services: web::Data<S>,
    req: HttpRequest,
) -> HttpResponse {
    let config = services.config();
    let Some(app) = config.connections.github.app.as_ref() else {
        return error_page(
            404,
            "Not Found",
            "No GitHub App is configured on this Automate instance.",
        );
    };

    match public_wizard_outcome(services.as_ref(), &req, app.acl.as_ref()) {
        PublicWizardOutcome::AdminOnly => return admin_only_page(),
        PublicWizardOutcome::Denied => return access_denied_page(),
        PublicWizardOutcome::Allowed => {}
    }

    let state = CsrfToken::new_random().secret().clone();
    let secure = crate::web::helpers::request::is_https(
        config.web.trust_proxy,
        req.headers(),
        req.uri().scheme_str(),
    );

    info!("Initiating GitHub App installation flow.");

    HttpResponse::Found()
        .cookie(wizard_state_cookie(WIZARD_PATH, state.clone(), secure))
        .append_header((
            actix_web::http::header::LOCATION,
            format!(
                "https://github.com/apps/{}/installations/new?state={}",
                urlencoding::encode(&app.slug),
                urlencoding::encode(&state),
            ),
        ))
        .finish()
}

async fn install_callback<S: Services + Send + Sync + 'static>(
    services: web::Data<S>,
    req: HttpRequest,
    query: web::Query<std::collections::HashMap<String, String>>,
) -> HttpResponse {
    let config = services.config();
    let Some(app) = config.connections.github.app.as_ref() else {
        return error_page(
            404,
            "Not Found",
            "No GitHub App is configured on this Automate instance.",
        );
    };

    // The same CSRF reasoning as the OAuth wizard: without it someone could walk
    // an admin through attaching an installation of the attacker's choosing.
    let expected_state = req
        .cookie(OAUTH_SETUP_STATE_COOKIE)
        .map(|c| c.value().to_string());
    if !state_matches(
        expected_state.as_deref(),
        query.get("state").map(String::as_str),
    ) {
        warn!("Rejected a GitHub App install callback with a missing or mismatched state.");
        return with_cleared_state(
            WIZARD_PATH,
            error_page(
                400,
                "Bad Request",
                "The installation could not be verified. Please start again.",
            ),
        );
    }

    // GitHub sends people back here after a cancelled install too.
    let Some(installation_id) = query
        .get("installation_id")
        .and_then(|id| id.parse::<u64>().ok())
    else {
        return with_cleared_state(
            WIZARD_PATH,
            html_page(
                200,
                "GitHub | Automate",
                "Nothing installed",
                "No installation was created. You can close this window and start again if that was not what you intended.",
            ),
        );
    };

    // Resolve the account through the App's own credentials rather than trusting
    // the query string, which the browser controls.
    let client = match crate::services::GitHubAppClient::new(app, services.http_client()) {
        Ok(client) => client,
        Err(err) => {
            error!("The configured GitHub App credentials are unusable: {err}");
            services.session().record_human_error(&err);
            return with_cleared_state(
                WIZARD_PATH,
                error_page(
                    500,
                    "Internal Server Error",
                    "This Automate instance's GitHub App credentials are not usable.",
                ),
            );
        }
    };

    let installation = match client.installations().await {
        Ok(installations) => installations.into_iter().find(|i| i.id == installation_id),
        Err(err) => {
            error!("Failed to confirm the new GitHub App installation: {err}");
            services.session().record_human_error(&err);
            return with_cleared_state(
                WIZARD_PATH,
                error_page(
                    502,
                    "Bad Gateway",
                    "We could not confirm the installation with GitHub. Please try again shortly.",
                ),
            );
        }
    };

    let Some(installation) = installation else {
        return with_cleared_state(
            WIZARD_PATH,
            error_page(
                400,
                "Bad Request",
                "GitHub does not report that installation as belonging to this app.",
            ),
        );
    };

    if let Err(err) = record_installation(&installation, services.as_ref()).await {
        error!("Failed to record the GitHub App installation: {err}");
        services.session().record_human_error(&err);
        return with_cleared_state(
            WIZARD_PATH,
            error_page(
                500,
                "Internal Server Error",
                "Failed to record the installation, please try again later.",
            ),
        );
    }

    info!(
        "Recorded GitHub App installation {} for '{}'.",
        installation.id, installation.account
    );

    with_cleared_state(
        WIZARD_PATH,
        html_page(
            200,
            "GitHub | Automate",
            "Installation complete",
            &format!(
                "Automate is now watching {}'s repositories. You can close this window.",
                installation.account
            ),
        ),
    )
}

/// `GET /api/v1/github/installations` — lists the connected accounts for the
/// admin SPA. Admin-gated by `api_auth`.
pub async fn list_installations<S: Services>(services: web::Data<S>) -> HttpResponse {
    match services
        .kv()
        .list::<GitHubInstallation>(INSTALLATIONS_PARTITION)
        .await
    {
        Ok(entries) => {
            HttpResponse::Ok().json(entries.into_iter().map(|(_, v)| v).collect::<Vec<_>>())
        }
        Err(err) => {
            error!("Failed to list GitHub App installations: {err}");
            services.session().record_human_error(&err);
            HttpResponse::InternalServerError().finish()
        }
    }
}
