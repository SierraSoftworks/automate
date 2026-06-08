use crate::prelude::*;

use super::Collector;

/// The European Central Bank's daily reference exchange rate feed.
///
/// The feed expresses every rate relative to the Euro (i.e. the number of units
/// of the foreign currency that one Euro buys), and is refreshed once per
/// working day at around 16:00 CET.
const DEFAULT_ECB_URL: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml";

/// A single exchange rate from the ECB feed, expressed as the number of units
/// of `currency` that one Euro buys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EcbExchangeRate {
    pub currency: String,
    pub rate: f64,
}

/// A collector which exposes the European Central Bank's daily reference
/// exchange rates and converts amounts between any two of the supported
/// currencies.
///
/// The parsed feed is cached for 24 hours (the ECB only publishes new rates
/// once per working day), so repeated conversions within a run – and across
/// runs on the same day – avoid re-fetching the upstream document.
pub struct EcbRateCollector {
    url: String,
}

impl Default for EcbRateCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl EcbRateCollector {
    pub fn new() -> Self {
        Self {
            url: DEFAULT_ECB_URL.to_string(),
        }
    }

    #[cfg(test)]
    pub fn new_with_url(url: impl ToString) -> Self {
        Self {
            url: url.to_string(),
        }
    }

    /// Converts `value` from the `from` currency into the `to` currency using
    /// the latest ECB reference rates.
    ///
    /// Both currency codes are matched case-insensitively, and `EUR` is always
    /// supported (it is the base of every rate). When the two currencies are
    /// the same the value is returned unchanged without fetching the feed.
    pub async fn convert(
        &self,
        services: &(impl Services + Send + Sync + 'static),
        value: f64,
        from: &str,
        to: &str,
    ) -> Result<f64, human_errors::Error> {
        let from = from.to_uppercase();
        let to = to.to_uppercase();

        if from == to {
            return Ok(value);
        }

        let rates = self.list(services).await?;

        let lookup = |code: &str| -> Result<f64, human_errors::Error> {
            if code == "EUR" {
                return Ok(1.0);
            }

            rates
                .iter()
                .find(|rate| rate.currency == code)
                .map(|rate| rate.rate)
                .ok_or_else(|| {
                    human_errors::user(
                        format!("The ECB feed does not provide an exchange rate for '{code}'."),
                        &[
                            "Check that the currency code is a valid ISO 4217 code.",
                            "The European Central Bank only publishes rates for a limited set of currencies.",
                        ],
                    )
                })
        };

        let from_rate = lookup(&from)?;
        let to_rate = lookup(&to)?;

        // Every rate is expressed relative to the Euro, so we convert the value
        // into Euros first and then into the target currency.
        Ok(value / from_rate * to_rate)
    }
}

#[async_trait::async_trait]
impl Collector for EcbRateCollector {
    type Item = EcbExchangeRate;

    /// Returns the EUR-relative exchange rates, fetching and parsing the ECB
    /// feed when the cached copy is older than 24 hours.
    #[instrument("collectors.ecb_rates.list", skip(self, services), err(Display))]
    async fn list(
        &self,
        services: &(impl Services + Send + Sync + 'static),
    ) -> Result<Vec<Self::Item>, human_errors::Error> {
        let client = services.http_client();
        let url = self.url.clone();

        services
            .cache()
            .cached(
                "ecb/rates",
                url.clone(),
                move || {
                    Box::pin(async move {
                        let response = client.get(&url).send().await.wrap_system_err(
                            format!("Failed to fetch exchange rates from the ECB feed at '{url}'."),
                            &[
                                "Check that the ECB feed is reachable from this host.",
                                "Check that your network connection is working properly.",
                            ],
                        )?;

                        let response = response.error_for_status().wrap_system_err(
                            "The ECB feed returned an unexpected error status.",
                            &["The European Central Bank service may be temporarily unavailable."],
                        )?;

                        let body = response.text().await.wrap_system_err(
                            "Failed to read the response body from the ECB feed.",
                            &["The European Central Bank service may be temporarily unavailable."],
                        )?;

                        parse_ecb_rates(&body)
                    })
                },
                chrono::Duration::hours(24),
            )
            .await
    }
}

/// The deserialised ECB envelope. Only the nested `Cube` hierarchy is of
/// interest; the `gesmes:` metadata elements are ignored.
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(rename = "Cube")]
    cube: CubeRoot,
}

#[derive(Debug, Deserialize)]
struct CubeRoot {
    /// The feed nests one dated `Cube` per day. The daily feed contains a
    /// single entry, but we accept several so the same parser works for the
    /// historical feeds.
    #[serde(rename = "Cube", default)]
    days: Vec<CubeDay>,
}

#[derive(Debug, Deserialize)]
struct CubeDay {
    #[serde(rename = "Cube", default)]
    rates: Vec<CubeRate>,
}

#[derive(Debug, Deserialize)]
struct CubeRate {
    #[serde(rename = "@currency")]
    currency: String,
    #[serde(rename = "@rate")]
    rate: f64,
}

/// Parses the ECB `eurofxref` XML document into a flat list of EUR-relative
/// exchange rates, using the most recent dated block in the feed.
fn parse_ecb_rates(xml: &str) -> Result<Vec<EcbExchangeRate>, human_errors::Error> {
    let envelope: Envelope = quick_xml::de::from_str(xml).wrap_system_err(
        "Failed to parse the exchange rate feed returned by the ECB.",
        &["The European Central Bank may have changed the format of its feed."],
    )?;

    let rates = envelope
        .cube
        .days
        .into_iter()
        .next()
        .map(|day| {
            day.rates
                .into_iter()
                .map(|rate| EcbExchangeRate {
                    currency: rate.currency.to_uppercase(),
                    rate: rate.rate,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if rates.is_empty() {
        return Err(human_errors::system(
            "The ECB feed did not contain any exchange rates.",
            &["The European Central Bank service may be temporarily unavailable."],
        ));
    }

    Ok(rates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::mock_services;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01" xmlns="http://www.ecb.int/vocabulary/2002-08-01/eurofxref">
    <gesmes:subject>Reference rates</gesmes:subject>
    <gesmes:Sender>
        <gesmes:name>European Central Bank</gesmes:name>
    </gesmes:Sender>
    <Cube>
        <Cube time="2024-01-15">
            <Cube currency="USD" rate="1.0945"/>
            <Cube currency="GBP" rate="0.86290"/>
            <Cube currency="JPY" rate="158.27"/>
        </Cube>
    </Cube>
</gesmes:Envelope>"#;

    #[test]
    fn parse_extracts_all_rates() {
        let rates = parse_ecb_rates(SAMPLE).expect("sample should parse");

        assert_eq!(rates.len(), 3);
        assert_eq!(
            rates[0],
            EcbExchangeRate {
                currency: "USD".to_string(),
                rate: 1.0945,
            }
        );
        assert_eq!(rates[2].currency, "JPY");
        assert_eq!(rates[2].rate, 158.27);
    }

    #[test]
    fn parse_rejects_empty_feed() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01" xmlns="http://www.ecb.int/vocabulary/2002-08-01/eurofxref">
    <Cube></Cube>
</gesmes:Envelope>"#;

        assert!(parse_ecb_rates(xml).is_err());
    }

    async fn mock_ecb() -> MockServer {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/eurofxref-daily.xml"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
            .mount(&mock_server)
            .await;
        mock_server
    }

    #[tokio::test]
    async fn list_returns_rates() {
        let mock_server = mock_ecb().await;
        let collector =
            EcbRateCollector::new_with_url(format!("{}/eurofxref-daily.xml", mock_server.uri()));
        let services = mock_services().await.unwrap();

        let rates = collector.list(&services).await.unwrap();
        assert_eq!(rates.len(), 3);
    }

    #[tokio::test]
    async fn convert_same_currency_is_identity() {
        let collector = EcbRateCollector::new_with_url("http://example.invalid/feed.xml");
        let services = mock_services().await.unwrap();

        // No HTTP request should be made when the currencies match.
        let result = collector
            .convert(&services, 123.45, "usd", "USD")
            .await
            .unwrap();
        assert_eq!(result, 123.45);
    }

    #[tokio::test]
    async fn convert_from_eur() {
        let mock_server = mock_ecb().await;
        let collector =
            EcbRateCollector::new_with_url(format!("{}/eurofxref-daily.xml", mock_server.uri()));
        let services = mock_services().await.unwrap();

        let result = collector
            .convert(&services, 100.0, "EUR", "USD")
            .await
            .unwrap();
        assert!((result - 109.45).abs() < 1e-9);
    }

    #[tokio::test]
    async fn convert_to_eur() {
        let mock_server = mock_ecb().await;
        let collector =
            EcbRateCollector::new_with_url(format!("{}/eurofxref-daily.xml", mock_server.uri()));
        let services = mock_services().await.unwrap();

        let result = collector
            .convert(&services, 109.45, "USD", "EUR")
            .await
            .unwrap();
        assert!((result - 100.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn convert_cross_currency() {
        let mock_server = mock_ecb().await;
        let collector =
            EcbRateCollector::new_with_url(format!("{}/eurofxref-daily.xml", mock_server.uri()));
        let services = mock_services().await.unwrap();

        // 109.45 USD -> 100 EUR -> 86.29 GBP
        let result = collector
            .convert(&services, 109.45, "USD", "GBP")
            .await
            .unwrap();
        assert!((result - 86.29).abs() < 1e-9);
    }

    #[tokio::test]
    async fn convert_unknown_currency_errors() {
        let mock_server = mock_ecb().await;
        let collector =
            EcbRateCollector::new_with_url(format!("{}/eurofxref-daily.xml", mock_server.uri()));
        let services = mock_services().await.unwrap();

        assert!(
            collector
                .convert(&services, 100.0, "USD", "ZZZ")
                .await
                .is_err()
        );
    }
}
