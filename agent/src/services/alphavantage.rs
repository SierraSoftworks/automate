use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::prelude::*;

/// The minimum interval between successive AlphaVantage requests. The free plan
/// permits at most one request per second, so requests are spaced at least this
/// far apart. The daily request quota is not enforced here; the general failure
/// handler retries once that limit is reached.
const MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(2);

/// A minimal AlphaVantage API client used to fetch stock quotes, company
/// overviews, and currency exchange rates.
///
/// AlphaVantage signals errors (invalid symbols, rate limiting, premium-only
/// endpoints) with a `200 OK` status and a structured JSON body rather than an
/// HTTP error, so every response is inspected for those markers.
///
/// Requests are rate limited to honour the free plan's one-request-per-second
/// limit. The limiter is shared across all clones of the client, so cloning the
/// client (as the per-request helpers do) does not bypass the limit.
#[derive(Clone)]
pub struct AlphaVantageClient {
    http_client: reqwest::Client,
    api_key: String,
    base_url: String,
    rate_limiter: Arc<RateLimiter>,
}

impl AlphaVantageClient {
    pub fn new(http_client: reqwest::Client, api_key: impl Into<String>) -> Self {
        Self {
            http_client,
            api_key: api_key.into(),
            base_url: "https://www.alphavantage.co".to_string(),
            rate_limiter: Arc::new(RateLimiter::new(MIN_REQUEST_INTERVAL)),
        }
    }

    #[cfg(test)]
    pub fn new_with_url(
        http_client: reqwest::Client,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            http_client,
            api_key: api_key.into(),
            base_url: base_url.into(),
            rate_limiter: Arc::new(RateLimiter::new(MIN_REQUEST_INTERVAL)),
        }
    }

    /// Overrides the minimum interval between requests. Used by tests to keep
    /// rate-limiting assertions fast.
    #[cfg(test)]
    pub fn with_min_request_interval(mut self, interval: Duration) -> Self {
        self.rate_limiter = Arc::new(RateLimiter::new(interval));
        self
    }

    /// Fetches the latest price for a ticker symbol via the `GLOBAL_QUOTE`
    /// endpoint, caching the result for 24 hours.
    pub async fn quote(
        &self,
        services: &(impl Services + Send + Sync + 'static),
        symbol: &str,
    ) -> Result<f64, human_errors::Error> {
        let client = self.clone();
        let symbol_owned = symbol.to_string();

        services
            .cache()
            .cached(
                "alphavantage/quote",
                symbol.to_string(),
                move || {
                    Box::pin(async move {
                        let body = client
                            .get(&[("function", "GLOBAL_QUOTE"), ("symbol", &symbol_owned)])
                            .await?;

                        let price = body
                            .get("Global Quote")
                            .and_then(|quote| quote.get("05. price"))
                            .and_then(|price| price.as_str())
                            .ok_or_else(|| {
                                human_errors::system(
                                    format!(
                                        "AlphaVantage did not return a price for '{symbol_owned}'."
                                    ),
                                    &[
                                        "Check that the ticker symbol is valid and recognised by AlphaVantage.",
                                        "The market may be closed or you may have exceeded your API rate limit.",
                                    ],
                                )
                            })?;

                        price.parse::<f64>().wrap_system_err(
                            format!("Failed to parse the price returned for '{symbol_owned}'."),
                            &["Report this issue to the development team on GitHub."],
                        )
                    })
                },
                chrono::Duration::hours(24),
            )
            .await
    }

    /// Looks up the trading currency for a symbol via the `OVERVIEW` endpoint,
    /// caching the result for six months (a symbol's currency rarely changes).
    /// Falls back to `USD` when AlphaVantage does not report a currency, which
    /// happens for non-equity instruments such as ETFs.
    pub async fn currency(
        &self,
        services: &(impl Services + Send + Sync + 'static),
        symbol: &str,
    ) -> Result<String, human_errors::Error> {
        let client = self.clone();
        let symbol_owned = symbol.to_string();

        services
            .cache()
            .cached(
                "alphavantage/overview",
                symbol.to_string(),
                move || {
                    Box::pin(async move {
                        let body = client
                            .get(&[("function", "OVERVIEW"), ("symbol", &symbol_owned)])
                            .await?;

                        let currency = body
                            .get("Currency")
                            .and_then(|currency| currency.as_str())
                            .filter(|currency| !currency.is_empty())
                            .unwrap_or("USD")
                            .to_uppercase();

                        Ok(currency)
                    })
                },
                chrono::Duration::days(182),
            )
            .await
    }

    /// Fetches the exchange rate from `from` to `to` via the
    /// `CURRENCY_EXCHANGE_RATE` endpoint, caching the result for 24 hours.
    /// Returns `1.0` when the currencies match.
    pub async fn exchange_rate(
        &self,
        services: &(impl Services + Send + Sync + 'static),
        from: &str,
        to: &str,
    ) -> Result<f64, human_errors::Error> {
        let from = from.to_uppercase();
        let to = to.to_uppercase();

        if from == to {
            return Ok(1.0);
        }

        let client = self.clone();
        let key = format!("{from}:{to}");

        services
            .cache()
            .cached(
                "alphavantage/forex",
                key,
                move || {
                    Box::pin(async move {
                        let body = client
                            .get(&[
                                ("function", "CURRENCY_EXCHANGE_RATE"),
                                ("from_currency", &from),
                                ("to_currency", &to),
                            ])
                            .await?;

                        let rate = body
                            .get("Realtime Currency Exchange Rate")
                            .and_then(|rate| rate.get("5. Exchange Rate"))
                            .and_then(|rate| rate.as_str())
                            .ok_or_else(|| {
                                human_errors::system(
                                    format!(
                                        "AlphaVantage did not return an exchange rate from {from} to {to}."
                                    ),
                                    &["Check that both currency codes are valid ISO currency codes."],
                                )
                            })?;

                        rate.parse::<f64>().wrap_system_err(
                            format!("Failed to parse the exchange rate from {from} to {to}."),
                            &["Report this issue to the development team on GitHub."],
                        )
                    })
                },
                chrono::Duration::hours(24),
            )
            .await
    }

    /// Performs a `GET /query` request against the AlphaVantage API and returns
    /// the parsed JSON body, surfacing the structured error responses
    /// AlphaVantage returns with a `200 OK` status (invalid symbols, rate
    /// limiting, and premium-only endpoints).
    async fn get(&self, params: &[(&str, &str)]) -> Result<serde_json::Value, human_errors::Error> {
        let url = format!("{}/query", self.base_url);

        let mut query = params.to_vec();
        query.push(("apikey", &self.api_key));

        // Honour the free plan's one-request-per-second limit before issuing
        // the request.
        self.rate_limiter.acquire().await;

        let response = self
            .http_client
            .get(&url)
            .query(&query)
            .send()
            .await
            .wrap_system_err(
                "Failed to contact the AlphaVantage API.",
                &[
                    "Check that the AlphaVantage API is reachable from this host.",
                    "Check that your network connection is working properly.",
                ],
            )?;

        let response = response.error_for_status().wrap_system_err(
            "The AlphaVantage API returned an unexpected error status.",
            &["The AlphaVantage service may be temporarily unavailable."],
        )?;

        let body: serde_json::Value = response.json().await.wrap_system_err(
            "Failed to parse the response from the AlphaVantage API.",
            &["The AlphaVantage service may have returned an unexpected response."],
        )?;

        if let Some(message) = body.get("Error Message").and_then(|value| value.as_str()) {
            return Err(human_errors::user(
                format!("AlphaVantage rejected the request: {message}"),
                &["Check that the ticker symbol or currency codes are valid."],
            ));
        }

        // `Note` is returned when the request rate limit is exceeded;
        // `Information` is returned when an endpoint requires a premium
        // subscription. Both are transient from the job's perspective, so they
        // are treated as system errors that allow the job to retry later.
        if let Some(message) = body
            .get("Note")
            .or_else(|| body.get("Information"))
            .and_then(|value| value.as_str())
        {
            return Err(human_errors::system(
                format!("AlphaVantage could not service the request: {message}"),
                &[
                    "You may have exceeded your AlphaVantage API rate limit; the job will retry later.",
                ],
            ));
        }

        Ok(body)
    }
}

/// A simple asynchronous rate limiter that spaces successive operations at
/// least `min_interval` apart. Multiple callers serialise on the internal
/// mutex, so concurrent requests queue up rather than racing.
struct RateLimiter {
    min_interval: Duration,
    last_request: Mutex<Option<Instant>>,
}

impl RateLimiter {
    fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last_request: Mutex::new(None),
        }
    }

    /// Waits until enough time has elapsed since the previous call to keep
    /// successive requests at least `min_interval` apart, then records the
    /// current time as the most recent request. The mutex is held across the
    /// wait so concurrent callers are spaced out rather than all proceeding at
    /// once.
    async fn acquire(&self) {
        let mut last_request = self.last_request.lock().await;

        if let Some(previous) = *last_request {
            let elapsed = previous.elapsed();
            if elapsed < self.min_interval {
                tokio::time::sleep(self.min_interval - elapsed).await;
            }
        }

        *last_request = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::mock_services;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mock_alphavantage(function: &str, body: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/query"))
            .and(query_param("function", function))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn quote_returns_price() {
        let server = mock_alphavantage(
            "GLOBAL_QUOTE",
            r#"{"Global Quote":{"01. symbol":"MSFT","05. price":"425.2700"}}"#,
        )
        .await;
        let services = mock_services().await.unwrap();
        let client = AlphaVantageClient::new_with_url(services.http_client(), "demo", server.uri());

        let price = client.quote(&services, "MSFT").await.unwrap();
        assert!((price - 425.27).abs() < 1e-9);
    }

    #[tokio::test]
    async fn quote_surfaces_invalid_symbol_error() {
        let server =
            mock_alphavantage("GLOBAL_QUOTE", r#"{"Error Message":"Invalid API call."}"#).await;
        let services = mock_services().await.unwrap();
        let client = AlphaVantageClient::new_with_url(services.http_client(), "demo", server.uri());

        assert!(client.quote(&services, "NOPE").await.is_err());
    }

    #[tokio::test]
    async fn quote_surfaces_rate_limit_note() {
        let server = mock_alphavantage(
            "GLOBAL_QUOTE",
            r#"{"Note":"Thank you for using Alpha Vantage! Our standard API rate limit is 25 requests per day."}"#,
        )
        .await;
        let services = mock_services().await.unwrap();
        let client = AlphaVantageClient::new_with_url(services.http_client(), "demo", server.uri());

        assert!(client.quote(&services, "MSFT").await.is_err());
    }

    #[tokio::test]
    async fn quote_surfaces_premium_information() {
        let server = mock_alphavantage(
            "GLOBAL_QUOTE",
            r#"{"Information":"This is a premium endpoint."}"#,
        )
        .await;
        let services = mock_services().await.unwrap();
        let client = AlphaVantageClient::new_with_url(services.http_client(), "demo", server.uri());

        assert!(client.quote(&services, "MSFT").await.is_err());
    }

    #[tokio::test]
    async fn currency_reads_overview() {
        let server = mock_alphavantage(
            "OVERVIEW",
            r#"{"Symbol":"VOD.LON","Currency":"GBP","Name":"Vodafone"}"#,
        )
        .await;
        let services = mock_services().await.unwrap();
        let client = AlphaVantageClient::new_with_url(services.http_client(), "demo", server.uri());

        let currency = client.currency(&services, "VOD.LON").await.unwrap();
        assert_eq!(currency, "GBP");
    }

    #[tokio::test]
    async fn currency_defaults_to_usd_when_absent() {
        let server = mock_alphavantage("OVERVIEW", r#"{}"#).await;
        let services = mock_services().await.unwrap();
        let client = AlphaVantageClient::new_with_url(services.http_client(), "demo", server.uri());

        let currency = client.currency(&services, "SPY").await.unwrap();
        assert_eq!(currency, "USD");
    }

    #[tokio::test]
    async fn exchange_rate_returns_rate() {
        let server = mock_alphavantage(
            "CURRENCY_EXCHANGE_RATE",
            r#"{"Realtime Currency Exchange Rate":{"1. From_Currency Code":"USD","3. To_Currency Code":"GBP","5. Exchange Rate":"0.79000"}}"#,
        )
        .await;
        let services = mock_services().await.unwrap();
        let client = AlphaVantageClient::new_with_url(services.http_client(), "demo", server.uri());

        let rate = client.exchange_rate(&services, "USD", "GBP").await.unwrap();
        assert!((rate - 0.79).abs() < 1e-9);
    }

    #[tokio::test]
    async fn exchange_rate_same_currency_is_identity_without_request() {
        // No mock server is configured, so any HTTP request would fail; the
        // matching-currency short-circuit must avoid it entirely.
        let services = mock_services().await.unwrap();
        let client = AlphaVantageClient::new_with_url(
            services.http_client(),
            "demo",
            "http://example.invalid",
        );

        let rate = client.exchange_rate(&services, "usd", "USD").await.unwrap();
        assert_eq!(rate, 1.0);
    }

    #[tokio::test]
    async fn requests_are_rate_limited() {
        let server = mock_alphavantage(
            "GLOBAL_QUOTE",
            r#"{"Global Quote":{"01. symbol":"MSFT","05. price":"1.0000"}}"#,
        )
        .await;
        let services = mock_services().await.unwrap();
        let interval = Duration::from_millis(200);
        let client = AlphaVantageClient::new_with_url(services.http_client(), "demo", server.uri())
            .with_min_request_interval(interval);

        // The first request proceeds immediately; the next two must each wait
        // for the interval to elapse, so three requests take at least two
        // intervals in total.
        let start = Instant::now();
        for _ in 0..3 {
            client.get(&[("function", "GLOBAL_QUOTE")]).await.unwrap();
        }
        let elapsed = start.elapsed();

        assert!(
            elapsed >= interval * 2,
            "expected at least {:?} to elapse across three requests, got {elapsed:?}",
            interval * 2
        );
    }
}
