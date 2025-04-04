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

/////////////////////////
/// add task trait
pub trait TaskTrait {
    #[doc = "Export task output to context."]
    fn export(&self) -> ::std::option::Option<&Export>;
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    fn if_(&self) -> ::std::option::Option<&String>;
    #[doc = "Configure the task's input."]
    fn input(&self) -> ::std::option::Option<&Input>;
    #[doc = "Holds additional information about the task."]
    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value>;
    #[doc = "Configure the task's output."]
    fn output(&self) -> ::std::option::Option<&Output>;
    #[doc = "The flow directive to be performed upon completion of the task."]
    fn then(&self) -> ::std::option::Option<&FlowDirective>;
    fn timeout(&self) -> ::std::option::Option<&TaskTimeout>;
}

impl TaskTrait for CallTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        self.timeout.as_ref()
    }
}
impl TaskTrait for DoTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        self.timeout.as_ref()
    }
}

impl TaskTrait for EmitTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        // Convert TaskTimeout to CallTaskAsyncApiTimeout if needed
        self.timeout.as_ref()
    }
}

impl TaskTrait for ForTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        // Convert TaskTimeout to CallTaskAsyncApiTimeout if needed
        self.timeout.as_ref()
    }
}

impl TaskTrait for ForkTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        // Convert TaskTimeout to CallTaskAsyncApiTimeout if needed
        self.timeout.as_ref()
    }
}

impl TaskTrait for RaiseTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        // Convert TaskTimeout to CallTaskAsyncApiTimeout if needed
        self.timeout.as_ref()
    }
}

impl TaskTrait for RunTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        // Convert TaskTimeout to CallTaskAsyncApiTimeout if needed
        self.timeout.as_ref()
    }
}
impl TaskTrait for SetTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        // Convert TaskTimeout to CallTaskAsyncApiTimeout if needed
        self.timeout.as_ref()
    }
}
impl TaskTrait for SwitchTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        self.timeout.as_ref()
    }
}
impl TaskTrait for TryTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        self.timeout.as_ref()
    }
}
impl TaskTrait for WaitTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        self.if_.as_ref()
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        self.input.as_ref()
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        &self.metadata
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        self.output.as_ref()
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        self.then.as_ref()
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        self.timeout.as_ref()
    }
}

impl TaskTrait for Task {
    fn export(&self) -> ::std::option::Option<&Export> {
        match self {
            Task::CallTask(t) => t.export(),
            Task::ForkTask(t) => t.export(),
            Task::EmitTask(t) => t.export(),
            Task::ForTask(t) => t.export(), // Needs to be matched before DoTask
            Task::DoTask(t) => t.export(),
            Task::RaiseTask(t) => t.export(),
            Task::RunTask(t) => t.export(),
            Task::SetTask(t) => t.export(),
            Task::SwitchTask(t) => t.export(),
            Task::TryTask(t) => t.export(),
            Task::WaitTask(t) => t.export(),
        }
    }

    fn if_(&self) -> ::std::option::Option<&::std::string::String> {
        match self {
            Task::CallTask(t) => t.if_(),
            Task::ForkTask(t) => t.if_(),
            Task::EmitTask(t) => t.if_(),
            Task::ForTask(t) => t.if_(), // Needs to be matched before DoTask
            Task::DoTask(t) => t.if_(),
            Task::RaiseTask(t) => t.if_(),
            Task::RunTask(t) => t.if_(),
            Task::SetTask(t) => t.if_(),
            Task::SwitchTask(t) => t.if_(),
            Task::TryTask(t) => t.if_(),
            Task::WaitTask(t) => t.if_(),
        }
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        match self {
            Task::CallTask(t) => t.input(),
            Task::ForkTask(t) => t.input(),
            Task::EmitTask(t) => t.input(),
            Task::ForTask(t) => t.input(), // Needs to be matched before DoTask
            Task::DoTask(t) => t.input(),
            Task::RaiseTask(t) => t.input(),
            Task::RunTask(t) => t.input(),
            Task::SetTask(t) => t.input(),
            Task::SwitchTask(t) => t.input(),
            Task::TryTask(t) => t.input(),
            Task::WaitTask(t) => t.input(),
        }
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        match self {
            Task::CallTask(t) => t.metadata(),
            Task::ForkTask(t) => t.metadata(),
            Task::EmitTask(t) => t.metadata(),
            Task::ForTask(t) => t.metadata(),
            Task::DoTask(t) => t.metadata(),
            Task::RaiseTask(t) => t.metadata(),
            Task::RunTask(t) => t.metadata(),
            Task::SetTask(t) => t.metadata(),
            Task::SwitchTask(t) => t.metadata(),
            Task::TryTask(t) => t.metadata(),
            Task::WaitTask(t) => t.metadata(),
        }
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        match self {
            Task::CallTask(t) => t.output(),
            Task::ForkTask(t) => t.output(),
            Task::EmitTask(t) => t.output(),
            Task::ForTask(t) => t.output(),
            Task::DoTask(t) => t.output(),
            Task::RaiseTask(t) => t.output(),
            Task::RunTask(t) => t.output(),
            Task::SetTask(t) => t.output(),
            Task::SwitchTask(t) => t.output(),
            Task::TryTask(t) => t.output(),
            Task::WaitTask(t) => t.output(),
        }
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        match self {
            Task::CallTask(t) => t.then(),
            Task::ForkTask(t) => t.then(),
            Task::EmitTask(t) => t.then(),
            Task::ForTask(t) => t.then(),
            Task::DoTask(t) => t.then(),
            Task::RaiseTask(t) => t.then(),
            Task::RunTask(t) => t.then(),
            Task::SetTask(t) => t.then(),
            Task::SwitchTask(t) => t.then(),
            Task::TryTask(t) => t.then(),
            Task::WaitTask(t) => t.then(),
        }
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        match self {
            Task::CallTask(t) => t.timeout(),
            Task::ForkTask(t) => t.timeout(),
            Task::EmitTask(t) => t.timeout(),
            Task::ForTask(t) => t.timeout(),
            Task::DoTask(t) => t.timeout(),
            Task::RaiseTask(t) => t.timeout(),
            Task::RunTask(t) => t.timeout(),
            Task::SetTask(t) => t.timeout(),
            Task::SwitchTask(t) => t.timeout(),
            Task::TryTask(t) => t.timeout(),
            Task::WaitTask(t) => t.timeout(),
        }
    }
}

//////////////////////////////////////////
/// add default implementations
///
impl Default for DoTask {
    fn default() -> Self {
        Self {
            do_: TaskList(vec![]),
            export: Default::default(),
            if_: Default::default(),
            input: Default::default(),
            metadata: Default::default(),
            output: Default::default(),
            then: Default::default(),
            timeout: Default::default(),
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
