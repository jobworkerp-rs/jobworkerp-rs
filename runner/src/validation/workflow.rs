use anyhow::Result;
use async_trait::async_trait;
use serde_json;
use std::sync::LazyLock;
use tracing;

// Common Workflow Validator (shared between app-wrapper and runner)
static WORKFLOW_VALIDATOR: LazyLock<Option<jsonschema::Validator>> = LazyLock::new(|| {
    let schema_content = include_str!("../../../runner/schema/workflow_minimal_fix.json");
    let schema = serde_json::from_str(schema_content)
        .inspect_err(|e| tracing::error!("Failed to parse workflow schema: {:?}", e))
        .ok()?;
    std::thread::spawn(move || {
        jsonschema::draft202012::new(&schema)
            .inspect_err(|e| tracing::warn!("Failed to create workflow schema validator: {:?}", e))
    })
    .join()
    .ok()?
    .ok()
});

#[async_trait]
pub trait WorkflowValidator: std::fmt::Debug {
    async fn validate_workflow(&self, definition: &serde_json::Value) -> Result<()>;
    async fn validate_input_schema(
        &self,
        definition: &serde_json::Value,
        input: &str,
    ) -> Result<()>;
}

#[derive(Debug)]
pub struct StandardWorkflowValidator;

#[async_trait]
impl WorkflowValidator for StandardWorkflowValidator {
    async fn validate_workflow(&self, definition: &serde_json::Value) -> Result<()> {
        tracing::debug!("Validating workflow schema for definition");
        if let Some(validator) = &*WORKFLOW_VALIDATOR {
            let mut error_details = Vec::new();
            for error in validator.iter_errors(definition) {
                error_details.push(format!(
                    "Path: {}, Message: {:#?}",
                    error.instance_path(),
                    error
                ));
            }
            if !error_details.is_empty() {
                tracing::warn!(
                    "Workflow schema validation failed: {}",
                    error_details.join("; ")
                );
                return Err(anyhow::anyhow!(
                    "Failed to validate workflow schema: errors: {}",
                    error_details.join("; ")
                ));
            }
        } else {
            tracing::warn!("Workflow schema validator is not initialized, skipping validation");
        }
        Ok(())
    }

    async fn validate_input_schema(
        &self,
        definition: &serde_json::Value,
        input: &str,
    ) -> Result<()> {
        // Input schema validation (implement as needed)
        let _ = (definition, input); // Avoid unused parameter warning
        Ok(())
    }
}

// Existing functional interface for app-wrapper (for compatibility)
pub async fn validate_workflow_schema(instance: &serde_json::Value) -> Result<()> {
    StandardWorkflowValidator.validate_workflow(instance).await
}

// Migration function for app-wrapper/src/workflow/definition.rs
pub fn get_workflow_validator() -> &'static Option<jsonschema::Validator> {
    &WORKFLOW_VALIDATOR
}
