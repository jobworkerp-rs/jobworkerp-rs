//! Standard AG-UI context values.

use serde::{Deserialize, Serialize};

/// A client-provided context item. Context never selects server execution settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Context {
    pub description: String,
    pub value: String,
}

impl Context {
    pub fn new(description: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            value: value.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_context_round_trips_and_rejects_extensions() {
        let context = Context::new("page", "example");
        let json = serde_json::to_value(&context).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"description":"page", "value":"example"})
        );
        assert_eq!(serde_json::from_value::<Context>(json).unwrap(), context);
        assert!(
            serde_json::from_str::<Context>(
                r#"{"description":"page","value":"x","type":"custom"}"#
            )
            .is_err()
        );
    }
}
