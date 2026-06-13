//! Filtering DSL used to conditionally process jobs and gate access.
//!
//! The lexer, parser and interpreter all live in the external [`filt_rs`]
//! crate, which is re-exported here wholesale. The only thing this module adds
//! is the [`serde_json::Value`] conversion that the orphan rule prevents us
//! from implementing directly on the [`filt_rs`] types.

pub use filt_rs::{Filter, FilterValue, Filterable};

/// Converts a [`serde_json::Value`] into a [`FilterValue`], borrowing strings
/// from the source value to avoid allocations.
///
/// Scalars (strings, numbers, booleans) and arrays are mapped onto their
/// equivalent filter values, with arrays becoming [`FilterValue::Tuple`]s. JSON
/// `null` and nested objects are mapped onto [`FilterValue::Null`] because the
/// filter DSL has no representation for structured records.
pub fn json_to_filter_value(value: &serde_json::Value) -> FilterValue<'_> {
    match value {
        serde_json::Value::Null => FilterValue::Null,
        serde_json::Value::Bool(b) => FilterValue::Bool(*b),
        serde_json::Value::Number(n) => n
            .as_f64()
            .map(FilterValue::Number)
            .unwrap_or(FilterValue::Null),
        serde_json::Value::String(s) => s.as_str().into(),
        serde_json::Value::Array(items) => {
            FilterValue::Tuple(items.iter().map(json_to_filter_value).collect())
        }
        serde_json::Value::Object(_) => FilterValue::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestObject {
        name: String,
    }

    impl Filterable for TestObject {
        fn get(&self, key: &str) -> FilterValue<'_> {
            match key {
                // Borrow rather than clone so the evaluation is zero-alloc.
                "name" => self.name.as_str().into(),
                _ => FilterValue::Null,
            }
        }
    }

    #[test]
    fn default_matches_everything() {
        let obj = TestObject {
            name: "John Doe".to_string(),
        };

        assert!(
            Filter::default()
                .matches(&obj)
                .expect("the default filter should evaluate")
        );
    }

    #[test]
    fn new_and_matches() {
        let obj = TestObject {
            name: "John Doe".to_string(),
        };

        let filter = Filter::new("name == \"John Doe\"").expect("parse filter");
        assert!(filter.matches(&obj).expect("run filter"));
        assert!(
            !Filter::new("name == \"Jane Doe\"")
                .expect("parse filter")
                .matches(&obj)
                .expect("run filter")
        );
    }

    #[test]
    fn serde_round_trip() {
        let filter: Filter =
            serde_json::from_str("\"name == \\\"John Doe\\\"\"").expect("deserialize filter");
        assert_eq!(filter.raw(), "name == \"John Doe\"");
        assert_eq!(
            serde_json::to_string(&filter).expect("serialize filter"),
            "\"name == \\\"John Doe\\\"\""
        );
    }

    #[test]
    fn json_conversion() {
        assert_eq!(
            json_to_filter_value(&serde_json::json!("hello")),
            FilterValue::from("hello")
        );
        assert_eq!(
            json_to_filter_value(&serde_json::json!(true)),
            FilterValue::Bool(true)
        );
        assert_eq!(
            json_to_filter_value(&serde_json::json!(42)),
            FilterValue::Number(42.0)
        );
        assert_eq!(
            json_to_filter_value(&serde_json::json!(null)),
            FilterValue::Null
        );
        assert_eq!(
            json_to_filter_value(&serde_json::json!({"nested": true})),
            FilterValue::Null
        );
        assert_eq!(
            json_to_filter_value(&serde_json::json!(["a", "b"])),
            FilterValue::Tuple(vec![FilterValue::from("a"), FilterValue::from("b")])
        );
    }
}
