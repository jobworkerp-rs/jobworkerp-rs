// Workflow definition types and utilities
// Submodule declarations and re-exports

// Submodule declarations
pub mod expression_cache;
pub mod expression_context;
pub mod liquid_globals;
pub mod liquid_tags;
pub mod transform;

// Allow clippy warnings for auto-generated workflow module
#[allow(clippy::derivable_impls)]
#[allow(clippy::clone_on_copy)]
#[allow(clippy::large_enum_variant)]
pub mod workflow;

// Re-exports for convenience
pub use expression_context::ExpressionContext;
pub use transform::*;
pub use workflow::errors::{ErrorCode, ErrorFactory};
pub use workflow::{DoTask, Document, Error, Task, WorkflowSchema};
