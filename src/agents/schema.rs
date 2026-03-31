use serde_json::Value;

/// Returned when an agent's response fails to parse or validate.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ParseError {}

/// Returns `value[field]` or a `ParseError` if the field is absent.
/// Used by future specialist agents (M7+) for structured response parsing.
#[allow(dead_code)]
pub fn require_field<'a>(value: &'a Value, field: &str) -> Result<&'a Value, ParseError> {
    value.get(field).ok_or_else(|| ParseError {
        message: format!("missing required field '{field}'"),
    })
}

/// Returns `value[field]` as `&str`, or a `ParseError` if absent or not a string.
/// Used by future specialist agents (M7+) for structured response parsing.
#[allow(dead_code)]
pub fn require_str<'a>(value: &'a Value, field: &str) -> Result<&'a str, ParseError> {
    require_field(value, field)?.as_str().ok_or_else(|| ParseError {
        message: format!("field '{field}' is not a string"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn require_field_returns_value_when_present() {
        let v = json!({"key": 42});
        assert_eq!(require_field(&v, "key").unwrap(), &json!(42));
    }

    #[test]
    fn require_field_returns_error_when_missing() {
        let v = json!({"other": 1});
        let err = require_field(&v, "key").unwrap_err();
        assert!(err.message.contains("key"));
    }

    #[test]
    fn require_str_returns_str_for_string_field() {
        let v = json!({"name": "alice"});
        assert_eq!(require_str(&v, "name").unwrap(), "alice");
    }

    #[test]
    fn require_str_returns_error_for_non_string_field() {
        let v = json!({"count": 5});
        let err = require_str(&v, "count").unwrap_err();
        assert!(err.message.contains("not a string"));
    }
}
