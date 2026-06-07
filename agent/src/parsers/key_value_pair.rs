use std::collections::HashMap;

/// Parses a string of whitespace-separated `key=value` pairs into a map.
///
/// Values may be wrapped in double quotes to allow them to contain whitespace,
/// for example `name="hello world"`. The surrounding quotes are stripped from
/// the resulting value. Tokens that do not contain an `=` are ignored, as are
/// pairs with an empty key. When a key appears more than once the last value
/// wins.
///
/// # Example
///
/// ```
/// use automate::parsers::parse_key_value_pairs;
///
/// let pairs = parse_key_value_pairs("MSFT=100 payee_name=\"Stock Market\"");
/// assert_eq!(pairs.get("MSFT").map(String::as_str), Some("100"));
/// assert_eq!(pairs.get("payee_name").map(String::as_str), Some("Stock Market"));
/// ```
pub fn parse_key_value_pairs(input: &str) -> HashMap<String, String> {
    let mut pairs = HashMap::new();

    for token in tokenize(input) {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };

        let key = key.trim();
        if key.is_empty() {
            continue;
        }

        pairs.insert(key.to_string(), unquote(value.trim()).to_string());
    }

    pairs
}

/// Splits a string into whitespace-separated tokens, treating text within
/// double quotes as a single token (so quoted values may contain spaces).
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for c in input.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// Strips a single pair of surrounding double quotes from a value, if present.
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_pairs() {
        let pairs = parse_key_value_pairs("a=1 b=2 c=3");
        assert_eq!(pairs.get("a").map(String::as_str), Some("1"));
        assert_eq!(pairs.get("b").map(String::as_str), Some("2"));
        assert_eq!(pairs.get("c").map(String::as_str), Some("3"));
    }

    #[test]
    fn respects_quoted_values() {
        let pairs = parse_key_value_pairs("a=1 name=\"hello world\" b=2");
        assert_eq!(pairs.get("a").map(String::as_str), Some("1"));
        assert_eq!(pairs.get("name").map(String::as_str), Some("hello world"));
        assert_eq!(pairs.get("b").map(String::as_str), Some("2"));
    }

    #[test]
    fn ignores_tokens_without_equals() {
        let pairs = parse_key_value_pairs("standalone a=1");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs.get("a").map(String::as_str), Some("1"));
    }

    #[test]
    fn ignores_empty_keys() {
        let pairs = parse_key_value_pairs("=value a=1");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs.get("a").map(String::as_str), Some("1"));
    }

    #[test]
    fn last_value_wins_for_duplicate_keys() {
        let pairs = parse_key_value_pairs("a=1 a=2");
        assert_eq!(pairs.get("a").map(String::as_str), Some("2"));
    }

    #[test]
    fn returns_empty_for_blank_input() {
        assert!(parse_key_value_pairs("").is_empty());
        assert!(parse_key_value_pairs("   ").is_empty());
    }
}
