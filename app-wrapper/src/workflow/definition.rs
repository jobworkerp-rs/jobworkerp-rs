// Compatibility layer: Re-export workflow definitions from infra crate
// This file provides backward compatibility for existing code that imports from app-wrapper

// Re-export all types from infra for compatibility
pub use infra::workflow::definition::workflow::{
    self, DoTask, Document, Error, FlowDirective, SwitchCase, Task, WorkflowSchema,
};

pub use infra::workflow::{
    RUNNER_SETTINGS_METADATA_LABEL, UseLoadUrlOrPath, WORKER_NAME_METADATA_LABEL,
    WORKER_PARAMS_METADATA_LABEL, WorkflowLoader, WorkflowPosition,
};

pub use infra::workflow::definition::workflow::errors::{ErrorCode, ErrorFactory};

pub use infra::workflow::definition::transform;
