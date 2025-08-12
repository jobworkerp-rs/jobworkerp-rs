use anyhow::Result;
use async_trait::async_trait;
use serde_json;
use std::sync::LazyLock;
use tracing;

// 共通のWorkflow Validator（app-wrapperとrunnerで共有）
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
                    error.instance_path, error
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
        // 入力スキーマバリデーション（必要に応じて実装）
        let _ = (definition, input); // unused parameter warning回避
        Ok(())
    }
}

// 既存のapp-wrapper用の関数形式インターフェース（互換性のため）
pub async fn validate_workflow_schema(instance: &serde_json::Value) -> Result<()> {
    StandardWorkflowValidator.validate_workflow(instance).await
}

// app-wrapper/src/workflow/definition.rs の migrate 用関数
pub fn get_workflow_validator() -> &'static Option<jsonschema::Validator> {
    &WORKFLOW_VALIDATOR
}
