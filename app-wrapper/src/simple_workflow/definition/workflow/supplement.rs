// cannot generate structure by typist (for serverless workflow version 1.0: generated from 1.0-alpha5)

use super::*;

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
        match self {
            CallTask::AsyncApi { export, .. }
            | CallTask::Grpc { export, .. }
            | CallTask::Http { export, .. }
            | CallTask::OpenApi { export, .. }
            | CallTask::Function { export, .. } => export.as_ref(),
        }
    }

    fn if_(&self) -> ::std::option::Option<&String> {
        match self {
            CallTask::AsyncApi { if_, .. }
            | CallTask::Grpc { if_, .. }
            | CallTask::Http { if_, .. }
            | CallTask::OpenApi { if_, .. }
            | CallTask::Function { if_, .. } => if_.as_ref(),
        }
    }

    fn input(&self) -> ::std::option::Option<&Input> {
        match self {
            CallTask::AsyncApi { input, .. }
            | CallTask::Grpc { input, .. }
            | CallTask::Http { input, .. }
            | CallTask::OpenApi { input, .. }
            | CallTask::Function { input, .. } => input.as_ref(),
        }
    }

    fn metadata(&self) -> &::serde_json::Map<::std::string::String, ::serde_json::Value> {
        match self {
            CallTask::AsyncApi { metadata, .. }
            | CallTask::Grpc { metadata, .. }
            | CallTask::Http { metadata, .. }
            | CallTask::OpenApi { metadata, .. }
            | CallTask::Function { metadata, .. } => metadata,
        }
    }

    fn output(&self) -> ::std::option::Option<&Output> {
        match self {
            CallTask::AsyncApi { output, .. }
            | CallTask::Grpc { output, .. }
            | CallTask::Http { output, .. }
            | CallTask::OpenApi { output, .. }
            | CallTask::Function { output, .. } => output.as_ref(),
        }
    }

    fn then(&self) -> ::std::option::Option<&FlowDirective> {
        match self {
            CallTask::AsyncApi { then, .. }
            | CallTask::Grpc { then, .. }
            | CallTask::Http { then, .. }
            | CallTask::OpenApi { then, .. }
            | CallTask::Function { then, .. } => then.as_ref(),
        }
    }

    fn timeout(&self) -> ::std::option::Option<&TaskTimeout> {
        match self {
            CallTask::AsyncApi { timeout, .. } => timeout.as_ref(),
            _ => None,
        }
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

impl TaskTrait for ListenTask {
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
            Task::ListenTask(t) => t.export(),
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
            Task::ListenTask(t) => t.if_(),
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
            Task::ListenTask(t) => t.input(),
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
            Task::ListenTask(t) => t.metadata(),
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
            Task::ListenTask(t) => t.output(),
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
            Task::ListenTask(t) => t.then(),
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
            Task::ListenTask(t) => t.timeout(),
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
            // dsl: Default::default(),
            metadata: Default::default(),
            name: Default::default(),
            // namespace: Default::default(),
            summary: Default::default(),
            tags: Default::default(),
            title: Default::default(),
            // version: Default::default(),
        }
    }
}
impl Default for WorkflowDsl {
    fn default() -> Self {
        Self("0.0.1".into())
    }
}
impl Default for WorkflowNamespace {
    fn default() -> Self {
        Self("default".into())
    }
}
impl Default for WorkflowVersion {
    fn default() -> Self {
        Self("0.0.1".into())
    }
}
impl Default for WorkflowName {
    fn default() -> Self {
        Self("default-workflow".into())
    }
}
