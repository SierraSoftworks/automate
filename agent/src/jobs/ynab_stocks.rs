use std::collections::HashMap;
use std::fmt::Display;

use rust_ynab::{Account, ClearedStatus, Client, NewTransaction, PlanId};
use uuid::Uuid;

use crate::parsers::parse_key_value_pairs;
use crate::prelude::*;
use crate::services::AlphaVantageClient;

#[derive(Clone, Serialize, Deserialize)]
pub struct YnabStocksConfig {
    /// The YNAB budget (plan) whose stock accounts should be synchronised.
    pub budget: Uuid,
}

impl Display for YnabStocksConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ynab-stocks/{}", self.budget)
    }
}

#[derive(Clone)]
pub struct YnabStocksWorkflow;

crate::register_job!(YnabStocksWorkflow);

/// The persisted synchronisation state for a single budget. The account mirror
/// allows us to fetch only the changes since our last run by supplying the
/// stored server knowledge token to YNAB.
#[derive(Clone, Serialize, Deserialize, Default)]
struct StocksState {
    #[serde(default)]
    server_knowledge: Option<i64>,
    #[serde(default)]
    accounts: HashMap<Uuid, Account>,
}

/// The parsed `/automate:stock` directive from an account note.
#[derive(Debug, PartialEq)]
struct StockSpec {
    /// The (ticker symbol, quantity held) pairs.
    holdings: Vec<(String, f64)>,
    /// The raw cost-basis specification (e.g. `USD5000` or `5000`), if provided.
    cost_basis: Option<String>,
    /// The capital gains tax rate, expressed as a fraction (e.g. `0.4` for 40%).
    cgt_rate: f64,
    /// The payee name to use for adjustment transactions, if overridden.
    payee_name: Option<String>,
}

/// The valuation of a single holding, used both for the net-value calculation
/// and for building a human-readable transaction memo.
struct StockValue {
    symbol: String,
    native_currency: String,
    native_price: f64,
    native_value: f64,
    /// The value of the holding converted into the budget's currency.
    value: f64,
}

impl Job for YnabStocksWorkflow {
    type JobType = YnabStocksConfig;

    fn partition() -> &'static str {
        "ynab/stocks"
    }

    /// Visibility timeout / retry backoff. This job calls the YNAB API and
    /// AlphaVantage for quotes and exchange rates, both of which are rate
    /// limited, so a failed run backs off for a long time before retrying.
    fn timeout(&self) -> chrono::TimeDelta {
        chrono::TimeDelta::hours(1)
    }

    #[instrument("workflow.ynab_stocks.setup", skip(self, services), err(Display))]
    async fn setup(
        &self,
        services: impl Services + Send + Sync + 'static,
    ) -> Result<(), human_errors::Error> {
        let config = services.config();
        CronJob::schedule(&config.workflows.ynab_stocks, services).await
    }

    #[instrument("workflow.ynab_stocks.handle", skip(self, ctx, job), fields(job = %job))]
    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        job: &Self::JobType,
    ) -> Result<(), human_errors::Error> {
        let services = ctx.services();
        let config = services.config();

        let api_key = config.connections.ynab.api_key.as_deref().ok_or_else(|| {
            human_errors::user(
                "No YNAB API key has been configured.",
                &[
                    "Set `connections.ynab.api_key` in your configuration file (for example via the YNAB_API_KEY environment variable).",
                    "Create a Personal Access Token from your YNAB account settings.",
                ],
            )
        })?;

        let client = Client::new(api_key).wrap_user_err(
            "We could not initialise the YNAB API client.",
            &["Check that your YNAB Personal Access Token is valid."],
        )?;

        let alphavantage_api_key =
            config.connections.alphavantage.api_key.as_deref().ok_or_else(|| {
                human_errors::user(
                    "No AlphaVantage API key has been configured.",
                    &[
                        "Set `connections.alphavantage.api_key` in your configuration file (for example via the ALPHAVANTAGE_API_KEY environment variable).",
                        "Claim a free API key from https://www.alphavantage.co/support/#api-key.",
                    ],
                )
            })?;
        let alphavantage = AlphaVantageClient::new(services.http_client(), alphavantage_api_key);

        let budget = job.budget;
        let plan = PlanId::Id(budget);

        let kv = services.kv();
        let mut state: StocksState = kv
            .get("ynab/stocks/state", budget.to_string())
            .await?
            .unwrap_or_default();

        // Fetch only the accounts that have changed since our last run by
        // supplying the stored server knowledge token to YNAB.
        let request = client.get_accounts(plan);
        let request = match state.server_knowledge {
            Some(sk) => request.with_server_knowledge(sk),
            None => request,
        };
        let (changed, new_knowledge) = request.send().await.wrap_system_err(
            format!("Failed to fetch the accounts for YNAB budget '{budget}'."),
            &[
                "Check that the budget ID is correct.",
                "Check that your YNAB API key has access to this budget.",
            ],
        )?;

        for account in changed {
            if account.deleted {
                state.accounts.remove(&account.id);
            } else {
                state.accounts.insert(account.id, account);
            }
        }
        state.server_knowledge = Some(new_knowledge);
        kv.set("ynab/stocks/state", budget.to_string(), state.clone())
            .await?;

        let settings = client.get_plan_settings(plan).await.wrap_system_err(
            format!("Failed to fetch the settings for YNAB budget '{budget}'."),
            &["Check that the budget ID is correct."],
        )?;
        let budget_currency = settings.currency_format.iso_code;

        for account in state.accounts.values() {
            if account.closed {
                continue;
            }

            let Some(note) = account.note.as_deref() else {
                continue;
            };
            let Some(spec) = parse_stock_command(note) else {
                continue;
            };

            // Value each holding in the budget's currency.
            let mut values = Vec::with_capacity(spec.holdings.len());
            for (symbol, quantity) in &spec.holdings {
                let price = alphavantage.quote(services, symbol).await?;
                let currency = alphavantage.currency(services, symbol).await?;
                let rate = alphavantage
                    .exchange_rate(services, &currency, &budget_currency)
                    .await?;
                let native_value = quantity * price;
                values.push(StockValue {
                    symbol: symbol.clone(),
                    native_currency: currency,
                    native_price: price,
                    native_value,
                    value: native_value * rate,
                });
            }

            if values.is_empty() {
                continue;
            }

            let gross: f64 = values.iter().map(|v| v.value).sum();

            // Convert the cost basis (which may be quoted in any currency) into
            // the budget's currency before applying the CGT deduction.
            let cost_basis = match spec.cost_basis.as_deref() {
                Some(raw) => match parse_currency_value(raw) {
                    Some((Some(ccy), amount)) => {
                        let rate = alphavantage
                            .exchange_rate(services, &ccy, &budget_currency)
                            .await?;
                        amount * rate
                    }
                    Some((None, amount)) => amount,
                    None => 0.0,
                },
                None => 0.0,
            };

            let net = net_value(gross, cost_basis, spec.cgt_rate);
            let shift = compute_shift(net, account.balance);

            // 1000 milliunits == 1 unit of the budget's currency, so we ignore
            // sub-unit fluctuations to avoid spamming the account with tiny
            // corrections.
            if shift.abs() <= 1000 {
                tracing::info!(
                    account = %account.name,
                    shift,
                    "Skipping stock adjustment because the change is below the minimum threshold."
                );
                continue;
            }

            let payee_name = spec
                .payee_name
                .clone()
                .unwrap_or_else(|| "Stock Market".to_string());

            client
                .create_transaction(
                    plan,
                    NewTransaction {
                        account_id: account.id,
                        date: chrono::Utc::now().date_naive(),
                        amount: shift,
                        payee_id: None,
                        payee_name: Some(payee_name),
                        category_id: None,
                        memo: Some(build_memo(&values)),
                        cleared: Some(ClearedStatus::Cleared),
                        approved: Some(true),
                        flag_color: None,
                        import_id: None,
                        subtransactions: None,
                    },
                )
                .await
                .wrap_system_err(
                    format!(
                        "Failed to record a stock value adjustment for account '{}'.",
                        account.name
                    ),
                    &["Check that your YNAB API key has write access to this budget."],
                )?;

            tracing::info!(
                account = %account.name,
                shift,
                "Recorded a stock value adjustment transaction."
            );
        }

        Ok(())
    }
}

/// Computes the net portfolio value after applying a capital gains tax
/// deduction. The deduction is `max((gross - cost_basis) * cgt_rate, 0)`,
/// mirroring the behaviour of the original automation.
fn net_value(gross: f64, cost_basis: f64, cgt_rate: f64) -> f64 {
    let cgt = ((gross - cost_basis) * cgt_rate).max(0.0);
    gross - cgt
}

/// Computes the adjustment (in milliunits) required to bring an account with
/// the given balance up to the target net value.
fn compute_shift(net: f64, current_balance: i64) -> i64 {
    let net_milliunits = (net * 1000.0).floor() as i64;
    net_milliunits - current_balance
}

/// Builds a human-readable memo summarising the valued holdings, truncated to
/// fit within YNAB's memo length limit.
fn build_memo(values: &[StockValue]) -> String {
    let memo = values
        .iter()
        .map(|v| {
            format!(
                "{}: {} {:.2} @ {} {:.2}",
                v.symbol, v.native_currency, v.native_value, v.native_currency, v.native_price
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    truncate_memo(&memo)
}

/// Truncates a memo to 500 characters (YNAB's limit), appending an ellipsis
/// when truncation occurs.
fn truncate_memo(memo: &str) -> String {
    if memo.chars().count() <= 500 {
        return memo.to_string();
    }

    let truncated: String = memo.chars().take(500 - 1).collect();
    format!("{truncated}…")
}

/// Parses a `/automate:stock` directive from an account note. Returns `None`
/// when the note does not contain the directive or specifies no holdings.
fn parse_stock_command(note: &str) -> Option<StockSpec> {
    let start = note.find("/automate:stock")?;
    let rest = &note[start + "/automate:stock".len()..];
    // The directive occupies a single line.
    let line = rest.lines().next().unwrap_or("");

    let mut holdings = Vec::new();
    let mut cost_basis = None;
    let mut cgt_rate = 0.0;
    let mut payee_name = None;

    for (key, value) in parse_key_value_pairs(line) {
        match key.as_str() {
            "cost_basis" => cost_basis = Some(value),
            "cgt_rate" => {
                cgt_rate = value
                    .trim_end_matches('%')
                    .trim()
                    .parse::<f64>()
                    .unwrap_or(0.0)
                    / 100.0;
            }
            "payee_name" => payee_name = Some(value),
            symbol => {
                if let Ok(quantity) = value.parse::<f64>() {
                    holdings.push((symbol.to_string(), quantity));
                }
            }
        }
    }

    if holdings.is_empty() {
        return None;
    }

    Some(StockSpec {
        holdings,
        cost_basis,
        cgt_rate,
        payee_name,
    })
}

/// Parses a currency-qualified amount such as `USD5000` or `5000`, returning
/// the optional currency code and the numeric amount.
fn parse_currency_value(input: &str) -> Option<(Option<String>, f64)> {
    let input = input.trim();
    let split = input.find(|c: char| c.is_ascii_digit() || c == '.')?;
    let (currency, amount) = input.split_at(split);

    let amount = amount.parse::<f64>().ok()?;
    let currency = if currency.is_empty() {
        None
    } else {
        Some(currency.to_string())
    };

    Some((currency, amount))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// Sorts holdings by symbol so assertions don't depend on the
    /// non-deterministic ordering produced by the underlying hash map.
    fn sorted_holdings(mut holdings: Vec<(String, f64)>) -> Vec<(String, f64)> {
        holdings.sort_by(|a, b| a.0.cmp(&b.0));
        holdings
    }

    #[test]
    fn parses_full_stock_command() {
        let note = "Some preamble\n/automate:stock MSFT=100 AAPL=50 cost_basis=USD5000 cgt_rate=40% payee_name=\"My Broker\"\nTrailing notes";
        let spec = parse_stock_command(note).expect("directive should parse");

        assert_eq!(
            sorted_holdings(spec.holdings),
            vec![("AAPL".to_string(), 50.0), ("MSFT".to_string(), 100.0)]
        );
        assert_eq!(spec.cost_basis.as_deref(), Some("USD5000"));
        assert!((spec.cgt_rate - 0.4).abs() < f64::EPSILON);
        assert_eq!(spec.payee_name.as_deref(), Some("My Broker"));
    }

    #[test]
    fn parses_minimal_stock_command() {
        let spec = parse_stock_command("/automate:stock GOOG=10").expect("directive should parse");

        assert_eq!(spec.holdings, vec![("GOOG".to_string(), 10.0)]);
        assert_eq!(spec.cost_basis, None);
        assert_eq!(spec.cgt_rate, 0.0);
        assert_eq!(spec.payee_name, None);
    }

    #[test]
    fn parses_fractional_quantities_and_dotted_symbols() {
        let spec =
            parse_stock_command("/automate:stock VOD.L=12.5 BRK.B=2").expect("directive parses");

        assert_eq!(
            sorted_holdings(spec.holdings),
            vec![("BRK.B".to_string(), 2.0), ("VOD.L".to_string(), 12.5)]
        );
    }

    #[test]
    fn returns_none_without_directive() {
        assert!(parse_stock_command("Just a regular note").is_none());
    }

    #[test]
    fn returns_none_without_holdings() {
        assert!(parse_stock_command("/automate:stock cost_basis=USD5000").is_none());
    }

    #[rstest]
    #[case("USD5000", Some("USD"), 5000.0)]
    #[case("5000", None, 5000.0)]
    #[case("EUR1234.56", Some("EUR"), 1234.56)]
    #[case(" GBP10 ", Some("GBP"), 10.0)]
    fn parses_currency_values(
        #[case] input: &str,
        #[case] expected_currency: Option<&str>,
        #[case] expected_amount: f64,
    ) {
        let (currency, amount) = parse_currency_value(input).expect("value should parse");
        assert_eq!(currency.as_deref(), expected_currency);
        assert!((amount - expected_amount).abs() < f64::EPSILON);
    }

    #[test]
    fn rejects_non_numeric_currency_values() {
        assert!(parse_currency_value("USD").is_none());
        assert!(parse_currency_value("").is_none());
    }

    #[test]
    fn net_value_applies_capital_gains_deduction() {
        // gross 10000, cost basis 5000, 40% CGT on the 5000 gain => 2000 deduction.
        let net = net_value(10_000.0, 5_000.0, 0.4);
        assert!((net - 8_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn net_value_never_applies_negative_deduction() {
        // gross below cost basis => no tax owed, net equals gross.
        let net = net_value(4_000.0, 5_000.0, 0.4);
        assert!((net - 4_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn net_value_without_cost_basis_taxes_full_gross() {
        let net = net_value(10_000.0, 0.0, 0.25);
        assert!((net - 7_500.0).abs() < f64::EPSILON);
    }

    #[rstest]
    #[case(100.0, 50_000, 50_000)]
    #[case(100.0, 150_000, -50_000)]
    #[case(100.0, 100_000, 0)]
    fn computes_shift_in_milliunits(#[case] net: f64, #[case] balance: i64, #[case] expected: i64) {
        assert_eq!(compute_shift(net, balance), expected);
    }

    #[test]
    fn truncate_memo_leaves_short_memos_untouched() {
        assert_eq!(truncate_memo("short"), "short");
    }

    #[test]
    fn truncate_memo_truncates_long_memos() {
        let memo = "x".repeat(500 + 10);
        let truncated = truncate_memo(&memo);
        assert_eq!(truncated.chars().count(), 500);
        assert!(truncated.ends_with('…'));
    }
}
