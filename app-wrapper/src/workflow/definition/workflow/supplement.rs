// cannot generate structure by typist (for serverless workflow version 1.0: generated from 1.0-alpha5)

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
                    // Convert days to u64 before multiplication to avoid overflow
                    if let Some(ms) = (*days as u64).checked_mul(24 * 60 * 60 * 1000) {
                        total_ms = total_ms.saturating_add(ms);
                    }
                }

                if let Some(hours) = hours {
                    // Convert hours to u64 before multiplication to avoid overflow
                    if let Some(ms) = (*hours as u64).checked_mul(60 * 60 * 1000) {
                        total_ms = total_ms.saturating_add(ms);
                    }
                }

                if let Some(minutes) = minutes {
                    // Convert minutes to u64 before multiplication to avoid overflow
                    if let Some(ms) = (*minutes as u64).checked_mul(60 * 1000) {
                        total_ms = total_ms.saturating_add(ms);
                    }
                }

                if let Some(seconds) = seconds {
                    // Convert seconds to u64 before multiplication to avoid overflow
                    if let Some(ms) = (*seconds as u64).checked_mul(1000) {
                        total_ms = total_ms.saturating_add(ms);
                    }
                }

                if let Some(milliseconds) = milliseconds {
                    // Add milliseconds directly, checking for overflow
                    total_ms = total_ms.saturating_add(*milliseconds as u64);
                }

                total_ms
            }
            Duration::Expression(expr) => {
                // Parse ISO 8601 duration expression
                // Format: P[n]Y[n]M[n]DT[n]H[n]M[n]S
                // P is the duration designator, T is the time designator

                let expr_str = expr.0.as_str();
                let mut total_ms = 0u64;

                // Split at the 'T' character to separate date and time parts
                let parts: Vec<&str> = expr_str.split('T').collect();
                let date_part = parts[0].strip_prefix('P').unwrap_or("");
                let time_part = if parts.len() > 1 { parts[1] } else { "" };

                // Parse date part (P[n]Y[n]M[n]W[n]D)
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

                // Parse time part ([n]H[n]M[n]S)
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
            do_: Default::default(),
            document: Default::default(),
            input: Default::default(),
            output: Default::default(),
        }
    }
}
