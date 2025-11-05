// Compatibility layer: Re-export workflow definitions from infra crate
// This file provides backward compatibility for existing code that imports from app-wrapper

// Re-export all types from infra for compatibility
pub use infra::workflow::definition::workflow::{
    self, DoTask, Document, Error, FlowDirective, SwitchCase, Task, WorkflowSchema,
};

pub use infra::workflow::{
    UseLoadUrlOrPath, WorkflowLoader, WorkflowPosition, RUNNER_SETTINGS_METADATA_LABEL,
    WORKER_NAME_METADATA_LABEL, WORKER_PARAMS_METADATA_LABEL,
};

pub use infra::workflow::definition::workflow::errors::{ErrorCode, ErrorFactory};

pub use infra::workflow::definition::transform;
