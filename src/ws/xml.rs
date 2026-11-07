//! XML: not there yet, answered as JSON.

use serde_json::Value;

#[must_use]
pub fn document(_: &str, value: &Value) -> String {
    value.to_string()
}

#[must_use]
pub fn error(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}
