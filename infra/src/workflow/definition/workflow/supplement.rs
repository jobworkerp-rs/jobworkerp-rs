// cannot generate structure by typist (for serverless workflow version 1.0: generated from 1.0-alpha5)
// Allow clippy warnings for manually maintained implementations of auto-generated types
// #![allow(clippy::derivable_impls)]

use std::{collections::HashMap, sync::Arc};

use super::*;
use proto::jobworkerp::data::RetryPolicy as JobworkerpRetryPolicy;

// helper for schema
impl Schema {
    pub fn json_schema(&self) -> Option<&::serde_json::Value> {
        match self {
            Schema::Variant0 {
                document,
                format: format_,
            } => {
                if format_.is_empty() || format_.eq("json") {
                    Some(document)
                } else {
                    tracing::warn!(
                    "Schema::json_schema not implemented for non-json format (Schema::Variant0): {}",
                    format_
                );
                    None
                }
            }
            Schema::Variant1 {
                format: format_, ..
            } => {
                // TODO not implemented
                // let mut schema = ::serde_json::Map::new();
                // schema.insert("$ref".to_string(), resource.endpoint);
                // schema.into()
                tracing::warn!(
                    "Schema::json_schema not implemented for external resource (Schema::Variant1): {}",
                    format_
                );
                None
            }
        }
    }
}
impl Default for Document {
    fn default() -> Self {
        Self {
            dsl: Default::default(),
            metadata: Default::default(),
            name: Default::default(),
            namespace: Default::default(),
            summary: Default::default(),
            tags: Default::default(),
            title: Default::default(),
            version: Default::default(),
        }
    }
}
impl Default for WorkflowName {
    fn default() -> Self {
        Self("default-workflow".into())
    }
}
impl Duration {
    pub fn from_millis(milliseconds: u64) -> Self {
        let r = milliseconds;
        let ms = r % 1000;
        let r = r / 1000;
        let seconds = r % 60;
        let r = r / 60;
        let minutes = r % 60;
        let r = r / 60;
        let hours = r % 24;
        let days = r / 24;

        Duration::Inline {
            days: Some(days as i64),
            hours: Some(hours as i64),
            minutes: Some(minutes as i64),
            seconds: Some(seconds as i64),
            milliseconds: Some(ms as i64),
        }
    }
    pub fn to_millis(&self) -> u64 {
        match self {
            Duration::Inline {
                days,
                hours,
                minutes,
                seconds,
                milliseconds,
            } => {
                let mut total_ms: u64 = 0;

                if let Some(days) = days {
                    if let Some(ms) = (*days as u64).checked_mul(24 * 60 * 60 * 1000) {
                        total_ms = total_ms.saturating_add(ms);
                    }
                }

                if let Some(hours) = hours {
                    if let Some(ms) = (*hours as u64).checked_mul(60 * 60 * 1000) {
                        total_ms = total_ms.saturating_add(ms);
                    }
                }

                if let Some(minutes) = minutes {
                    if let Some(ms) = (*minutes as u64).checked_mul(60 * 1000) {
                        total_ms = total_ms.saturating_add(ms);
                    }
                }

                if let Some(seconds) = seconds {
                    if let Some(ms) = (*seconds as u64).checked_mul(1000) {
                        total_ms = total_ms.saturating_add(ms);
                    }
                }

                if let Some(milliseconds) = milliseconds {
                    total_ms = total_ms.saturating_add(*milliseconds as u64);
                }

                total_ms
            }
            Duration::Expression(expr) => {
                // Format: P[n]Y[n]M[n]DT[n]H[n]M[n]S
                // P is the duration designator, T is the time designator

                let expr_str = expr.0.as_str();
                let mut total_ms = 0u64;

                // Split at the 'T' character to separate date and time parts
                let parts: Vec<&str> = expr_str.split('T').collect();
                let date_part = parts[0].strip_prefix('P').unwrap_or("");
                let time_part = if parts.len() > 1 { parts[1] } else { "" };

                let mut current_number = String::new();
                for c in date_part.chars() {
                    if c.is_ascii_digit() || c == '.' {
                        current_number.push(c);
                    } else if !current_number.is_empty() {
                        let value = current_number.parse::<f64>().unwrap_or(0.0);
                        match c {
                            'Y' => {
                                // Approximate a year as 365.25 days
                                total_ms += (value * 365.25 * 24.0 * 60.0 * 60.0 * 1000.0) as u64;
                            }
                            'M' => {
                                // Approximate a month as 30.44 days
                                total_ms += (value * 30.44 * 24.0 * 60.0 * 60.0 * 1000.0) as u64;
                            }
                            'W' => {
                                // A week is exactly 7 days
                                total_ms += (value * 7.0 * 24.0 * 60.0 * 60.0 * 1000.0) as u64;
                            }
                            'D' => {
                                // A day is exactly 24 hours
                                total_ms += (value * 24.0 * 60.0 * 60.0 * 1000.0) as u64;
                            }
                            _ => {}
                        }
                        current_number.clear();
                    }
                }

                current_number.clear();
                for c in time_part.chars() {
                    if c.is_ascii_digit() || c == '.' {
                        current_number.push(c);
                    } else if !current_number.is_empty() {
                        let value = current_number.parse::<f64>().unwrap_or(0.0);
                        match c {
                            'H' => {
                                // An hour is exactly 60 minutes
                                total_ms += (value * 60.0 * 60.0 * 1000.0) as u64;
                            }
                            'M' => {
                                // A minute is exactly 60 seconds
                                total_ms += (value * 60.0 * 1000.0) as u64;
                            }
                            'S' => {
                                // A second is exactly 1000 milliseconds
                                total_ms += (value * 1000.0) as u64;
                            }
                            _ => {}
                        }
                        current_number.clear();
                    }
                }

                total_ms
            }
        }
    }
}
impl RetryPolicy {
    pub fn to_jobworkerp(self) -> JobworkerpRetryPolicy {
        JobworkerpRetryPolicy {
            r#type: self
                .backoff
                .map(|x| match x {
                    RetryBackoff::Constant(_) => {
                        proto::jobworkerp::data::RetryType::Constant as i32
                    }
                    RetryBackoff::Exponential(_) => {
                        proto::jobworkerp::data::RetryType::Exponential as i32
                    }
                    RetryBackoff::Linear(_) => proto::jobworkerp::data::RetryType::Linear as i32,
                })
                .unwrap_or_default(),
            interval: self.delay.map(|x| x.to_millis() as u32).unwrap_or_default(),
            max_retry: self
                .limit
                .as_ref()
                .and_then(|l| l.attempt.as_ref().and_then(|a| a.count.map(|i| i as u32)))
                .unwrap_or_default(),
            max_interval: self
                .limit
                .as_ref()
                .and_then(|l| {
                    l.attempt
                        .as_ref()
                        .and_then(|a| a.duration.as_ref().map(|d| d.to_millis() as u32))
                })
                .unwrap_or_default(),
            basis: 2.0, // XXX fixed
        }
    }
}

impl Default for WorkflowSchema {
    fn default() -> Self {
        Self {
            checkpointing: Default::default(),
            do_: Default::default(),
            document: Default::default(),
            input: Default::default(),
            output: Default::default(),
        }
    }
}
impl WorkflowSchema {
    pub fn create_do_task(&self, metadata: Arc<HashMap<String, String>>) -> DoTask {
        let mut meta_map = serde_json::Map::new();
        for (k, v) in metadata.iter() {
            meta_map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        DoTask {
            do_: self.do_.clone(),
            input: Some(self.input.clone()),
            output: self.output.clone(),
            metadata: meta_map,
            ..Default::default()
        }
    }
}

// Script runner helpers

/// Runtime language validation for script execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidatedLanguage {
    Python,
    Javascript,
}

impl ValidatedLanguage {
    /// Validate language string at runtime
    /// Note: Not implementing FromStr trait to avoid confusion with standard trait
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "python" => Ok(Self::Python),
            "javascript" | "js" => Ok(Self::Javascript),
            _ => Err(format!("Unsupported script language: {}", s)),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Javascript => "javascript",
        }
    }
}

/// Python-specific settings extracted from task metadata
///
/// Python-specific settings are stored in task metadata with the following keys:
/// - `python.version`: Python version (default: "3.12")
/// - `python.packages`: Comma-separated package list (e.g., "numpy,pandas")
/// - `python.requirements_url`: URL to requirements.txt file
#[derive(Debug, Clone)]
pub struct PythonScriptSettings {
    pub version: String,
    pub packages: Vec<String>,
    pub requirements_url: Option<String>,
}

impl PythonScriptSettings {
    /// Extract from task metadata (recommended approach)
    ///
    /// # Example
    /// ```yaml
    /// metadata:
    ///   python.version: "3.12"
    ///   python.packages: "numpy,pandas"
    ///   python.requirements_url: "https://example.com/requirements.txt"
    /// ```
    pub fn from_metadata(metadata: &HashMap<String, String>) -> Result<Self, anyhow::Error> {
        let version = metadata
            .get("python.version")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "3.12".to_string());

        let packages: Vec<String> = metadata
            .get("python.packages")
            .map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let requirements_url = metadata
            .get("python.requirements_url")
            .map(|s| s.to_string());

        // Validation: packages and requirements_url are mutually exclusive
        if !packages.is_empty() && requirements_url.is_some() {
            return Err(anyhow::anyhow!(
                "python.packages and python.requirements_url are mutually exclusive"
            ));
        }

        Ok(Self {
            version,
            packages,
            requirements_url,
        })
    }
}

impl Default for PythonScriptSettings {
    fn default() -> Self {
        Self {
            version: "3.12".to_string(),
            packages: vec![],
            requirements_url: None,
        }
    }
}
