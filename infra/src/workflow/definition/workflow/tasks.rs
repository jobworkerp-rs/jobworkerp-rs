// Allow clippy warnings for manually maintained implementations of auto-generated types
// #![allow(clippy::derivable_impls)]

use super::*;

/////////////////////////
/// add task trait
pub trait TaskTrait {
    #[doc = "Set checkpoint for the task, if true the task will be checkpointed."]
    fn checkpoint(&self) -> bool;
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
    #[doc = "The task's timeout configuration, if any."]
    fn timeout(&self) -> ::std::option::Option<&TaskTimeout>;
    #[doc = "Returns the type of the task as a static string."]
    fn task_type(&self) -> &'static str;
}

impl TaskTrait for DoTask {
    fn checkpoint(&self) -> bool {
        self.checkpoint
    }
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
    fn task_type(&self) -> &'static str {
        "do"
    }
}

impl TaskTrait for ForTask {
    fn checkpoint(&self) -> bool {
        self.checkpoint
    }
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
    fn task_type(&self) -> &'static str {
        "for"
    }
}

impl TaskTrait for ForkTask {
    fn checkpoint(&self) -> bool {
        self.checkpoint
    }
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
    fn task_type(&self) -> &'static str {
        "fork"
    }
}

impl TaskTrait for RaiseTask {
    fn checkpoint(&self) -> bool {
        self.checkpoint
    }
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
    fn task_type(&self) -> &'static str {
        "raise"
    }
}

impl TaskTrait for RunTask {
    fn export(&self) -> ::std::option::Option<&Export> {
        self.export.as_ref()
    }
    fn checkpoint(&self) -> bool {
        self.checkpoint
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
    fn task_type(&self) -> &'static str {
        "run"
    }
}
impl TaskTrait for SetTask {
    fn checkpoint(&self) -> bool {
        self.checkpoint
    }
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
    fn task_type(&self) -> &'static str {
        "set"
    }
}
impl TaskTrait for SwitchTask {
    fn checkpoint(&self) -> bool {
        self.checkpoint
    }
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
    fn task_type(&self) -> &'static str {
        "switch"
    }
}
impl TaskTrait for TryTask {
    fn checkpoint(&self) -> bool {
        self.checkpoint
    }
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
    fn task_type(&self) -> &'static str {
        "try"
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
    fn checkpoint(&self) -> bool {
        self.checkpoint
    }
    fn task_type(&self) -> &'static str {
        "wait"
    }
}

impl TaskTrait for Task {
    fn export(&self) -> ::std::option::Option<&Export> {
        match self {
            // Task::CallTask(t) => t.export(),
            Task::ForkTask(t) => t.export(),
            // Task::EmitTask(t) => t.export(),
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
            // Task::CallTask(t) => t.if_(),
            Task::ForkTask(t) => t.if_(),
            // Task::EmitTask(t) => t.if_(),
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
            // Task::CallTask(t) => t.input(),
            Task::ForkTask(t) => t.input(),
            // Task::EmitTask(t) => t.input(),
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
            // Task::CallTask(t) => t.metadata(),
            Task::ForkTask(t) => t.metadata(),
            // Task::EmitTask(t) => t.metadata(),
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
            // Task::CallTask(t) => t.output(),
            Task::ForkTask(t) => t.output(),
            // Task::EmitTask(t) => t.output(),
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
            // Task::CallTask(t) => t.then(),
            Task::ForkTask(t) => t.then(),
            // Task::EmitTask(t) => t.then(),
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
            // Task::CallTask(t) => t.timeout(),
            Task::ForkTask(t) => t.timeout(),
            // Task::EmitTask(t) => t.timeout(),
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

    fn checkpoint(&self) -> bool {
        match self {
            // Task::CallTask(t) => t.checkpoint(),
            Task::ForkTask(t) => t.checkpoint,
            // Task::EmitTask(t) => t.checkpoint(),
            Task::ForTask(t) => t.checkpoint,
            Task::DoTask(t) => t.checkpoint,
            Task::RaiseTask(t) => t.checkpoint,
            Task::RunTask(t) => t.checkpoint,
            Task::SetTask(t) => t.checkpoint,
            Task::SwitchTask(t) => t.checkpoint,
            Task::TryTask(t) => t.checkpoint,
            Task::WaitTask(t) => t.checkpoint,
        }
    }

    fn task_type(&self) -> &'static str {
        match self {
            Task::ForkTask(_) => "fork",
            Task::ForTask(_) => "for",
            Task::DoTask(_) => "do",
            Task::RaiseTask(_) => "raise",
            Task::RunTask(_) => "run",
            Task::SetTask(_) => "set",
            Task::SwitchTask(_) => "switch",
            Task::TryTask(_) => "try",
            Task::WaitTask(_) => "wait",
        }
    }
}

//////////////////////////////////////////
/// add default implementations
///
impl Default for TaskList {
    fn default() -> Self {
        Self(vec![])
    }
}

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
            checkpoint: Default::default(),
        }
    }
}
impl Default for SetTask {
    fn default() -> Self {
        Self {
            export: Default::default(),
            if_: Default::default(),
            input: Default::default(),
            metadata: Default::default(),
            output: Default::default(),
            set: Default::default(),
            then: Default::default(),
            timeout: Default::default(),
            checkpoint: Default::default(),
        }
    }
}
impl Default for TryTask {
    fn default() -> Self {
        Self {
            try_: TaskList(vec![]),
            catch: Default::default(),
            export: Default::default(),
            if_: Default::default(),
            input: Default::default(),
            metadata: Default::default(),
            output: Default::default(),
            then: Default::default(),
            timeout: Default::default(),
            checkpoint: Default::default(),
        }
    }
}
