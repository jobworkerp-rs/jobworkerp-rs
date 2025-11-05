// WorkflowPosition: Represents the current execution position within a workflow
// Moved from app-wrapper/src/workflow/execute/context.rs to resolve circular dependency

use command_utils::util::stack::StackWithHistory;
use std::fmt;

/// Represents the current execution position within a workflow
///
/// Uses JSON Pointer (RFC 6901) format internally for path representation.
/// Each path segment can be either a String (task name) or Number (array index).
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct WorkflowPosition {
    pub path: StackWithHistory<serde_json::Value>,
}

impl WorkflowPosition {
    pub fn new(path: Vec<serde_json::Value>) -> Self {
        Self {
            path: StackWithHistory::new_with(path),
        }
    }

    /// Parse a JSON Pointer string (RFC 6901) into WorkflowPosition
    /// Each segment is converted to serde_json::Value (String or Number)
    pub fn parse(path: &str) -> anyhow::Result<Self> {
        if !path.starts_with('/') && !path.is_empty() {
            return Err(anyhow::anyhow!(
                "JSON pointer must start with '/' or be empty"
            ));
        }
        fn unescape_to_value(segment: &str) -> anyhow::Result<serde_json::Value> {
            let mut s = String::new();
            let mut chars = segment.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '~' {
                    match chars.next() {
                        Some('0') => s.push('~'),
                        Some('1') => s.push('/'),
                        Some(other) => {
                            s.push('~');
                            s.push(other);
                        }
                        None => s.push('~'),
                    }
                } else {
                    s.push(c);
                }
            }
            // Try to parse as number, otherwise as string
            if let Ok(n) = s.parse::<i64>() {
                Ok(serde_json::Value::Number(n.into()))
            } else {
                Ok(serde_json::Value::String(s))
            }
        }
        let segments = if path.is_empty() {
            Vec::new()
        } else {
            path[1..]
                .split('/')
                .map(unescape_to_value)
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(Self::new(segments))
    }

    pub fn push(&mut self, name: String) {
        self.path.push(serde_json::Value::String(name));
    }

    pub fn push_idx(&mut self, idx: u32) -> bool {
        serde_json::Number::from_u128(idx as u128)
            .map(|n| self.path.push(serde_json::Value::Number(n)))
            .is_some()
    }

    pub fn pop(&mut self) -> Option<serde_json::Value> {
        self.path.pop()
    }

    pub fn current(&self) -> Option<&serde_json::Value> {
        self.path.last()
    }

    pub fn last(&self) -> Option<&serde_json::Value> {
        self.path.last()
    }

    pub fn last_name(&self) -> Option<String> {
        self.path.last().and_then(|v| {
            if let serde_json::Value::String(s) = v {
                Some(s.clone())
            } else {
                None
            }
        })
    }

    pub fn full(&self) -> &[serde_json::Value] {
        self.path.snapshot()
    }

    // return the path relative to base_path
    // return empty vector if self.path is equal to base_path
    // if base_path is not a prefix of self.path, return None
    pub fn relative_path(&self, base_path: &[serde_json::Value]) -> Option<Vec<serde_json::Value>> {
        if self.path.len() < base_path.len() {
            return None; // self.path is shorter than base_path
        }
        // return the path relative to base_path
        let mut relative = Vec::new();
        for (i, v) in self.path.snapshot().iter().enumerate() {
            if i < base_path.len() && v != &base_path[i] {
                return None;
            } else if i >= base_path.len() {
                // only push if we are beyond the base_path
                relative.push(v.clone());
            }
        }
        Some(relative)
    }

    pub fn n_prev(&self, n: usize) -> Vec<serde_json::Value> {
        self.path.state_before_operations(n)
    }

    // rfc-6901
    pub fn as_json_pointer(&self) -> String {
        self.path.json_pointer()
    }

    // alias for errors.rs compatibility
    pub fn as_error_instance(&self) -> String {
        self.as_json_pointer()
    }
}

impl std::fmt::Display for WorkflowPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_json_pointer())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let pos = WorkflowPosition::parse("").unwrap();
        assert_eq!(pos.path.len(), 0);
    }

    #[test]
    fn test_parse_simple() {
        let mut pos = WorkflowPosition::parse("/foo/bar").unwrap();
        assert_eq!(
            pos.path.pop(),
            Some(serde_json::Value::String("bar".to_string()))
        );
        assert_eq!(
            pos.path.pop(),
            Some(serde_json::Value::String("foo".to_string()))
        );
        assert_eq!(pos.path.pop(), None);
    }

    #[test]
    fn test_parse_number() {
        let mut pos = WorkflowPosition::parse("/foo/42").unwrap();
        assert_eq!(pos.path.pop(), Some(serde_json::Value::Number(42.into())));
        assert_eq!(
            pos.path.pop(),
            Some(serde_json::Value::String("foo".to_string()))
        );
        assert_eq!(pos.path.pop(), None);
    }

    #[test]
    fn test_parse_escaped() {
        // ~0 -> ~, ~1 -> /
        let mut pos = WorkflowPosition::parse("/~0foo/~1bar/~0~1baz").unwrap();
        assert_eq!(
            pos.path.pop(),
            Some(serde_json::Value::String("~/baz".to_string()))
        );
        assert_eq!(
            pos.path.pop(),
            Some(serde_json::Value::String("/bar".to_string()))
        );
        assert_eq!(
            pos.path.pop(),
            Some(serde_json::Value::String("~foo".to_string()))
        );
        assert_eq!(pos.path.pop(), None);
    }

    #[test]
    fn test_parse_mixed() {
        let mut pos = WorkflowPosition::parse("/foo/0/~01/~10/~0~1").unwrap();
        assert_eq!(
            pos.path.pop(),
            Some(serde_json::Value::String("~/".to_string()))
        );
        assert_eq!(
            pos.path.pop(),
            Some(serde_json::Value::String("/0".to_string()))
        );
        assert_eq!(
            pos.path.pop(),
            Some(serde_json::Value::String("~1".to_string()))
        );
        assert_eq!(pos.path.pop(), Some(serde_json::Value::Number(0.into())));
        assert_eq!(
            pos.path.pop(),
            Some(serde_json::Value::String("foo".to_string()))
        );
        assert_eq!(pos.path.pop(), None);
    }

    #[test]
    fn test_parse_invalid() {
        // not starting with / and not empty
        assert!(WorkflowPosition::parse("foo/bar").is_err());
    }

    #[test]
    fn test_relative_path_same_position() {
        let pos = WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
        ]);
        let relative_path = pos.relative_path(pos.full()).unwrap();
        assert_eq!(relative_path, Vec::<serde_json::Value>::new());
    }

    #[test]
    fn test_relative_path_child() {
        let pos = WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
            serde_json::Value::String("task1".to_string()),
        ]);
        let base = vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
        ];
        let relative_path = pos.relative_path(&base).unwrap();
        assert_eq!(
            relative_path,
            vec![serde_json::Value::String("task1".to_string())]
        );
    }

    #[test]
    fn test_relative_path_mismatch() {
        let pos = WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
        ]);
        let base = vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step2".to_string()),
        ];
        let relative_path = pos.relative_path(&base);
        assert_eq!(relative_path, None);
    }
}
