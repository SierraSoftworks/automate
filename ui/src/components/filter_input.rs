//! A filter expression editor with immediate syntax and field diagnostics.

use std::collections::BTreeSet;

use filt_rs::{
    BinaryOperator, Expr, ExprVisitor, Filter, FilterValue, Function, Glob, LogicalOperator,
    UnaryOperator,
};
use yew::prelude::*;

use crate::components::TextArea;

#[derive(Properties, PartialEq)]
pub struct FilterInputProps {
    pub id: AttrValue,
    pub value: AttrValue,
    pub onchange: Callback<String>,

    #[prop_or_default]
    pub onblur: Callback<String>,

    /// The property names which the object being filtered exposes.
    #[prop_or_default]
    pub fields: Vec<String>,

    #[prop_or_default]
    pub disabled: bool,

    /// Whether the agent reported a problem with this field.
    #[prop_or_default]
    pub invalid: bool,
}

#[function_component(FilterInput)]
pub fn filter_input(props: &FilterInputProps) -> Html {
    let analysis = analyze_filter(props.value.as_str(), &props.fields);
    let has_syntax_error = matches!(analysis, FilterAnalysis::Invalid(_));

    let diagnostics = match analysis {
        FilterAnalysis::Empty => html! {},
        FilterAnalysis::Invalid(error) => html! {
            <p class="filter-input__message filter-input__message--error" role="status">
                { error }
            </p>
        },
        FilterAnalysis::Valid {
            used_fields,
            unsupported_fields,
        } => {
            let usage = if used_fields.is_empty() {
                "Valid expression. No fields are referenced.".to_string()
            } else {
                format!("Valid expression. Uses: {}.", used_fields.join(", "))
            };

            html! {
                <>
                    <p class="filter-input__message filter-input__message--valid" role="status">
                        { usage }
                    </p>
                    if !unsupported_fields.is_empty() {
                        <p class="filter-input__message filter-input__message--warning" role="status">
                            { unsupported_field_warning(&unsupported_fields) }
                        </p>
                    }
                </>
            }
        }
    };

    html! {
        <div class="filter-input">
            <TextArea
                id={props.id.clone()}
                value={props.value.clone()}
                onchange={props.onchange.clone()}
                onblur={props.onblur.clone()}
                placeholder={Some(AttrValue::from("title contains \"release\""))}
                rows={2}
                disabled={props.disabled}
                invalid={props.invalid || has_syntax_error}
                monospace={true}
            />
            { diagnostics }
            if !props.fields.is_empty() {
                <p class="filter-input__fields">
                    { "Available fields: " }
                    <code>{ props.fields.join(", ") }</code>
                </p>
            }
        </div>
    }
}

fn unsupported_field_warning(fields: &[String]) -> String {
    if fields.len() == 1 {
        format!("Unsupported field: {}.", fields[0])
    } else {
        format!("Unsupported fields: {}.", fields.join(", "))
    }
}

#[derive(Debug, PartialEq)]
enum FilterAnalysis {
    Empty,
    Invalid(String),
    Valid {
        used_fields: Vec<String>,
        unsupported_fields: Vec<String>,
    },
}

fn analyze_filter(expression: &str, supported_fields: &[String]) -> FilterAnalysis {
    if expression.trim().is_empty() {
        return FilterAnalysis::Empty;
    }

    let filter = match Filter::new(expression) {
        Ok(filter) => filter,
        Err(error) => return FilterAnalysis::Invalid(error.to_string()),
    };

    let mut collector = PropertyCollector::default();
    filter.visit(&mut collector);

    let used_fields: Vec<String> = collector
        .properties
        .into_iter()
        .map(str::to_owned)
        .collect();
    let supported_fields: BTreeSet<&str> = supported_fields.iter().map(String::as_str).collect();
    let unsupported_fields = used_fields
        .iter()
        .filter(|field| !supported_fields.contains(field.as_str()))
        .cloned()
        .collect();

    FilterAnalysis::Valid {
        used_fields,
        unsupported_fields,
    }
}

#[derive(Default)]
struct PropertyCollector<'a> {
    properties: BTreeSet<&'a str>,
}

impl<'a> ExprVisitor<'a, ()> for PropertyCollector<'a> {
    fn visit_literal(&mut self, _value: &'a FilterValue<'a>) {}

    fn visit_property(&mut self, name: &'a str) {
        self.properties.insert(name);
    }

    fn visit_function_call(&mut self, _function: &'a dyn Function, args: &'a [Expr<'a>]) {
        for arg in args {
            self.visit_expr(arg);
        }
    }

    fn visit_binary(&mut self, left: &'a Expr<'a>, _operator: BinaryOperator, right: &'a Expr<'a>) {
        self.visit_expr(left);
        self.visit_expr(right);
    }

    fn visit_logical(
        &mut self,
        left: &'a Expr<'a>,
        _operator: LogicalOperator,
        right: &'a Expr<'a>,
    ) {
        self.visit_expr(left);
        self.visit_expr(right);
    }

    fn visit_unary(&mut self, _operator: UnaryOperator, right: &'a Expr<'a>) {
        self.visit_expr(right);
    }

    fn visit_like(&mut self, left: &'a Expr<'a>, _glob: &'a Glob) {
        self.visit_expr(left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_fields_from_the_whole_expression() {
        let supported = vec!["title".to_string(), "draft".to_string(), "tag".to_string()];

        assert_eq!(
            analyze_filter(
                r#"trim(title) != "" && !draft && tag like "release-*""#,
                &supported,
            ),
            FilterAnalysis::Valid {
                used_fields: vec!["draft".to_string(), "tag".to_string(), "title".to_string()],
                unsupported_fields: vec![],
            }
        );
    }

    #[test]
    fn reports_unsupported_fields_once() {
        let supported = vec!["title".to_string()];

        assert_eq!(
            analyze_filter(r#"title == owner && owner != """#, &supported),
            FilterAnalysis::Valid {
                used_fields: vec!["owner".to_string(), "title".to_string()],
                unsupported_fields: vec!["owner".to_string()],
            }
        );
    }

    #[test]
    fn reports_invalid_and_empty_expressions() {
        assert!(matches!(
            analyze_filter("title ==", &[]),
            FilterAnalysis::Invalid(_)
        ));
        assert_eq!(analyze_filter("  ", &[]), FilterAnalysis::Empty);
    }
}
