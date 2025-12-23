use std::fmt::Display;

/// Interpolates template expressions in the format `${{ expression }}` within a string.
///
/// This function scans through the input string and replaces all occurrences of `${{ expression }}`
/// with the result of calling the provided handler function with the extracted expression.
///
/// Supports escaping via `\${{ }}` which will output `${{ }}` literally without interpolation.
///
/// # Arguments
///
/// * `input` - The input string containing template expressions
/// * `handler` - A function that takes an expression string and returns a Result with a value implementing `Display`
///
/// # Example
///
/// ```
/// use automate::parsers::interpolation::interpolate;
///
/// let input = "Hello ${{ name }}, you are ${{ age }} years old!";
/// let result = interpolate(input, |expr| {
///     match expr.trim() {
///         "name" => Ok("Alice".to_string()),
///         "age" => Ok("30".to_string()),
///         _ => Ok(format!("${{{{ {} }}}}", expr)),
///     }
/// }).unwrap();
/// assert_eq!(result, "Hello Alice, you are 30 years old!");
/// ```
pub fn interpolate<F, R>(input: &str, handler: F) -> Result<String, human_errors::Error>
where
    F: Fn(&str) -> Result<R, human_errors::Error>,
    R: Display,
{
    Parser::new(input).parse(handler)
}

/// A recursive descent parser for template interpolation.
struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    /// Creates a new parser for the given input.
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    /// Parses the entire input, applying the handler to each interpolation expression.
    fn parse<F, R>(mut self, handler: F) -> Result<String, human_errors::Error>
    where
        F: Fn(&str) -> Result<R, human_errors::Error>,
        R: Display,
    {
        let mut result = String::with_capacity(self.input.len());

        while !self.is_at_end() {
            if self.peek() == Some('\\') && self.peek_ahead(1) == Some('$') {
                self.parse_escaped_interpolation(&mut result);
            } else if self.peek() == Some('$') && self.peek_ahead(1) == Some('{') {
                self.parse_interpolation(&mut result, &handler)?;
            } else {
                self.parse_text(&mut result);
            }
        }

        Ok(result)
    }

    /// Parses an escaped interpolation expression `\${{ ... }}`.
    fn parse_escaped_interpolation(&mut self, output: &mut String) {
        // Consume the backslash
        self.advance();

        // Output the literal $ and continue parsing
        if let Some(ch) = self.advance() {
            output.push(ch);
        }
    }

    /// Parses an interpolation expression `${{ ... }}`.
    fn parse_interpolation<F, R>(
        &mut self,
        output: &mut String,
        handler: &F,
    ) -> Result<(), human_errors::Error>
    where
        F: Fn(&str) -> Result<R, human_errors::Error>,
        R: Display,
    {
        let start = self.pos;

        // Consume '$'
        self.advance();

        // Check for first '{'
        if self.peek() != Some('{') {
            output.push('$');
            return Ok(());
        }
        self.advance();

        // Check for second '{'
        if self.peek() != Some('{') {
            output.push('$');
            output.push('{');
            return Ok(());
        }
        self.advance();

        // Extract the expression
        let expr = self.parse_expression(start)?;
        let value = handler(expr)?;
        output.push_str(&value.to_string());
        Ok(())
    }

    /// Parses the expression content between `${{` and `}}`.
    fn parse_expression(&mut self, template_start: usize) -> Result<&'a str, human_errors::Error> {
        let expr_start = self.pos;
        let mut depth = 1;

        while !self.is_at_end() {
            match self.peek() {
                Some('{') => {
                    self.advance();
                    if self.peek() == Some('{') {
                        depth += 1;
                        self.advance();
                    }
                }
                Some('}') => {
                    if self.peek_ahead(1) == Some('}') {
                        if depth == 1 {
                            // Found the closing }}
                            let expr_end = self.pos;
                            self.advance(); // consume first '}'
                            self.advance(); // consume second '}'
                            return Ok(&self.input[expr_start..expr_end]);
                        } else {
                            depth -= 1;
                            self.advance(); // consume first '}'
                            self.advance(); // consume second '}'
                        }
                    } else {
                        self.advance();
                    }
                }
                _ => {
                    self.advance();
                }
            }
        }

        // No closing }} found - construct a helpful error message
        let preview_end = (expr_start + 20).min(self.input.len());
        let preview = &self.input[template_start..preview_end];
        let ellipsis = if preview_end < self.input.len() {
            "..."
        } else {
            ""
        };

        Err(human_errors::user(
            format!(
                "We could not find a closing `}}}}` for the expression starting at '{}{}'.",
                preview, ellipsis
            ),
            &[
                "Make sure that you have closed your expression completely, it should look like `${{ env.VAR }}`.",
                "Escape your expression using `\\${{...` if you don't wish to use interpolation.",
            ],
        ))
    }

    /// Parses regular text (anything that's not an interpolation or escape sequence).
    fn parse_text(&mut self, output: &mut String) {
        if let Some(ch) = self.advance() {
            output.push(ch);
        }
    }

    /// Returns the current character without consuming it.
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    /// Returns the character at the given offset ahead without consuming it.
    fn peek_ahead(&self, offset: usize) -> Option<char> {
        self.input[self.pos..].chars().nth(offset)
    }

    /// Advances the parser by one character and returns it.
    fn advance(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    /// Returns true if the parser has reached the end of the input.
    fn is_at_end(&self) -> bool {
        self.pos >= self.input.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// Helper function that maps simple variable names to values
    fn simple_handler(expr: &str) -> Result<String, human_errors::Error> {
        match expr.trim() {
            "name" => Ok("World".to_string()),
            "user" => Ok("Alice".to_string()),
            "age" => Ok("30".to_string()),
            "a" => Ok("1".to_string()),
            "b" => Ok("2".to_string()),
            "var" => Ok("VALUE".to_string()),
            "" => Ok("EMPTY".to_string()),
            _ => Ok("REPLACED".to_string()),
        }
    }

    #[rstest]
    #[case("Hello ${{ name }}!", "Hello World!")]
    #[case("User: ${{ user }}, Age: ${{ age }}", "User: Alice, Age: 30")]
    #[case("This is a plain string", "This is a plain string")]
    #[case("${{ a }}${{ b }}", "12")]
    #[case("Not a template ${ test }", "Not a template ${ test }")]
    #[case("${{}}", "EMPTY")]
    fn test_basic_interpolation(#[case] input: &str, #[case] expected: &str) {
        let result = interpolate(input, simple_handler).unwrap();
        assert_eq!(result, expected);
    }

    #[rstest]
    #[case(r"This is \${{ not interpolated }}", "This is ${{ not interpolated }}")]
    #[case(
        r"Normal: ${{ var }}, Escaped: \${{ literal }}",
        "Normal: VALUE, Escaped: ${{ literal }}"
    )]
    #[case(r"\${{ one }} and \${{ two }}", "${{ one }} and ${{ two }}")]
    #[case(r"\${{ escaped }}", "${{ escaped }}")]
    #[case(r"text \${{ escaped }}", "text ${{ escaped }}")]
    #[case(r"\$text", "$text")]
    fn test_escape_sequences(#[case] input: &str, #[case] expected: &str) {
        let result = interpolate(input, simple_handler).unwrap();
        assert_eq!(result, expected);
    }

    #[rstest]
    #[case("${{   spacey   }}", "   SPACEY   ")]
    #[case("${{ env.MY_VAR_123 }}", " ENV.MY_VAR_123 ")]
    #[case("${{ outer {{ inner }} }}", " OUTER {{ INNER }} ")]
    fn test_expression_formats(#[case] input: &str, #[case] expected: &str) {
        let result = interpolate(input, |expr| Ok(expr.to_uppercase())).unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_env_variable_pattern() {
        let input = "API_KEY=${{ env.API_KEY }}";
        let result = interpolate(input, |expr| {
            if expr.trim().starts_with("env.") {
                let var_name = expr.trim().trim_start_matches("env.");
                Ok(std::env::var(var_name).unwrap_or_else(|_| format!("${{{{ {} }}}}", expr)))
            } else {
                Ok(format!("${{{{ {} }}}}", expr))
            }
        })
        .unwrap();
        // The result will depend on whether API_KEY is set
        assert!(result.starts_with("API_KEY="));
    }

    #[rstest]
    #[case("Incomplete ${{ test", "closing `}}`")]
    #[case("${{ unclosed", "closing `}}`")]
    fn test_error_cases(#[case] input: &str, #[case] error_contains: &str) {
        let result = interpolate(input, simple_handler);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains(error_contains));
    }

    #[test]
    fn test_handler_error_propagation() {
        let input = "${{ error }}";
        let result = interpolate(input, |expr| {
            if expr.trim() == "error" {
                Err(human_errors::user(
                    "Handler rejected this expression",
                    &["This is a test error"],
                ))
            } else {
                Ok("OK")
            }
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Handler rejected"));
    }

    #[test]
    fn test_handler_validates_expression_format() {
        let input = "${{ env.VALID }} and ${{ invalid }}";
        let result = interpolate(input, |expr| {
            let trimmed = expr.trim();
            if trimmed.starts_with("env.") {
                Ok(trimmed.trim_start_matches("env.").to_string())
            } else {
                Err(human_errors::user(
                    format!("Invalid expression format: '{}'", trimmed),
                    &["Expressions must start with 'env.' prefix"],
                ))
            }
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Invalid expression format"));
    }
}
