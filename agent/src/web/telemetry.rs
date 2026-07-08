use std::{
    pin::Pin,
    task::{Context, Poll},
};

use actix_web::{dev::*, web};
use actix_web::{Error, http::header::HeaderMap};
use futures::{
    Future, FutureExt,
    future::{Ready, ok},
};
use opentelemetry::propagation::Extractor;
use tracing_batteries::prelude::*;

use crate::services::Services;

/// Query-string parameters whose values carry credentials or single-use secrets
/// and must never be written to a span. The most important here are the OAuth /
/// OIDC `code` and `state` returned on the auth callback.
const SENSITIVE_QUERY_PARAMS: &[&str] = &[
    "code",
    "state",
    "id_token",
    "access_token",
    "refresh_token",
    "token",
    "client_secret",
];

/// Request headers whose values carry credentials and must be redacted from the
/// span's header dump. `authorization` holds the bearer ID token; `cookie` still
/// carries the transient OAuth `state` cookie used by the setup wizard.
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
];

/// Renders the request target (path + query) for the span, redacting the values
/// of any [`SENSITIVE_QUERY_PARAMS`] so secrets such as the OIDC `code`/`state`
/// never land in telemetry. Parameter names are preserved so the shape of the
/// request is still legible.
fn redact_target(uri: &actix_web::http::Uri) -> String {
    match uri.query() {
        None => uri.path().to_string(),
        Some(query) => {
            let redacted = query
                .split('&')
                .map(|pair| {
                    let name = pair.split('=').next().unwrap_or(pair);
                    if SENSITIVE_QUERY_PARAMS
                        .iter()
                        .any(|p| name.eq_ignore_ascii_case(p))
                    {
                        format!("{name}=REDACTED")
                    } else {
                        pair.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("&");
            format!("{}?{}", uri.path(), redacted)
        }
    }
}

/// Renders the request headers for the span, redacting the values of any
/// [`SENSITIVE_HEADERS`] (the session cookie, authorization tokens, and the CSRF
/// secret) while keeping every header name for debugging.
fn redact_headers(headers: &HeaderMap) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            if SENSITIVE_HEADERS
                .iter()
                .any(|h| name.as_str().eq_ignore_ascii_case(h))
            {
                format!("{name}: REDACTED")
            } else {
                format!("{name}: {value:?}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct TracingLogger<S: Services + Clone + Send + Sync + 'static> {
    _services: std::marker::PhantomData<S>,
}

impl<S: Services + Clone + Send + Sync + 'static> TracingLogger<S> {
    pub fn new() -> Self {
        TracingLogger { _services: std::marker::PhantomData }
    }
}

impl<S, A, B> Transform<A, ServiceRequest> for TracingLogger<S>
where
    A: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    A::Future: 'static,
    S: Services + Clone + Send + Sync + 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = TracingLoggerMiddleware<S, A>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: A) -> Self::Future {
        ok(TracingLoggerMiddleware { service, _services: std::marker::PhantomData })
    }
}

#[doc(hidden)]
pub struct TracingLoggerMiddleware<S: Services + Clone + Send + Sync + 'static, A> {
    service: A,
    _services: std::marker::PhantomData<S>,
}

impl<S: Services + Clone + Send + Sync + 'static, A, B> Service<ServiceRequest> for TracingLoggerMiddleware<S, A>
where
    A: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    A::Future: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let user_agent = req
            .headers()
            .get("User-Agent")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");

        // Redact secrets before they reach the span: the request target can carry
        // the OAuth/OIDC `code` and `state` query parameters, and the header dump
        // would otherwise include the session cookie and any Authorization token.
        let http_target = redact_target(req.uri());
        let http_headers = redact_headers(req.headers());

        let span = info_span!(
            "request",
            "otel.kind" = "server",
            "otel.name" = req.match_pattern().unwrap_or_else(|| req.uri().path().to_string()),
            "net.transport" = "IP.TCP",
            "net.peer.ip" = %req.connection_info().realip_remote_addr().unwrap_or(""),
            "http.target" = %http_target,
            "http.user_agent" = %user_agent,
            "http.status_code" = EmptyField,
            "http.method" = %req.method(),
            "http.url" = %req.match_pattern().unwrap_or_else(|| req.path().into()),
            "http.headers" = %http_headers,
        );

        // Propagate OpenTelemetry parent span context information
        let context = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&HeaderMapExtractor::from(req.headers()))
        });

        let _ = span.set_parent(context);

        let services = req.app_data::<web::Data<S>>().cloned();

        let fut = self
            .service
            .call(req)
            .map(move |outcome| match &outcome {
                Ok(response) => {
                    Span::current()
                        .record("http.status_code", display(response.response().status()));
                    outcome
                }
                Err(error) => {
                    if let Some(services) = services {
                        let err = tracing_batteries::ErrorInfo::new(&error)
                            .with_metadata("http.target", http_target)
                            .with_metadata("http.status_code", error.as_response_error().status_code().as_u16().to_string());
                        services.session().record_custom_error(err);
                    }

                    Span::current().record(
                        "http.status_code",
                        display(error.as_response_error().status_code()),
                    );
                    outcome
                }
            })
            .instrument(span);

        Box::pin(fut)
    }
}

struct HeaderMapExtractor<'a> {
    headers: &'a HeaderMap,
}

impl<'a> From<&'a HeaderMap> for HeaderMapExtractor<'a> {
    fn from(headers: &'a HeaderMap) -> Self {
        HeaderMapExtractor { headers }
    }
}

impl<'a> Extractor for HeaderMapExtractor<'a> {
    fn get(&self, key: &str) -> Option<&'a str> {
        self.headers.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.headers.keys().map(|v| v.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::Uri;
    use actix_web::http::header::{HeaderName, HeaderValue};

    #[test]
    fn redact_target_redacts_oidc_code_and_state() {
        let uri: Uri = "/api/v1/auth/callback?code=secret-code&state=secret-state"
            .parse()
            .unwrap();
        assert_eq!(
            redact_target(&uri),
            "/api/v1/auth/callback?code=REDACTED&state=REDACTED"
        );
    }

    #[test]
    fn redact_target_keeps_non_sensitive_parameters() {
        let uri: Uri = "/api/v1/kv/cache?key=oidc%3Ajwks".parse().unwrap();
        assert_eq!(redact_target(&uri), "/api/v1/kv/cache?key=oidc%3Ajwks");

        let uri: Uri = "/admin".parse().unwrap();
        assert_eq!(redact_target(&uri), "/admin");

        // A bare flag parameter (no `=`) is preserved verbatim.
        let uri: Uri = "/?demo".parse().unwrap();
        assert_eq!(redact_target(&uri), "/?demo");
    }

    #[test]
    fn redact_headers_redacts_credentials_but_keeps_names() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("cookie"),
            HeaderValue::from_static("automate_session=super-secret-jwt"),
        );
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer super-secret"),
        );
        headers.insert(
            HeaderName::from_static("x-custom"),
            HeaderValue::from_static("visible"),
        );

        let rendered = redact_headers(&headers);
        assert!(rendered.contains("cookie: REDACTED"));
        assert!(rendered.contains("authorization: REDACTED"));
        assert!(!rendered.contains("super-secret"));
        assert!(rendered.contains("x-custom: \"visible\""));
    }
}
