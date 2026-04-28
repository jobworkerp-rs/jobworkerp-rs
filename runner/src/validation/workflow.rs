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

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml_to_json(s: &str) -> serde_json::Value {
        let v: serde_yaml::Value = serde_yaml::from_str(s).unwrap();
        serde_json::to_value(v).unwrap()
    }

    fn run_validate(s: &str) -> Result<()> {
        let v = yaml_to_json(s);
        let validator = WORKFLOW_VALIDATOR
            .as_ref()
            .expect("schema validator must initialize");
        let errs: Vec<_> = validator
            .iter_errors(&v)
            .map(|e| format!("{}: {}", e.instance_path(), e))
            .collect();
        if errs.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(errs.join("; ")))
        }
    }

    // Cases from docs/workflow-schema-validation-issues.md
    const CASE_A: &str = r#"
document: { dsl: "1.0.0-jobworkerp", namespace: t, name: a, version: "1.0.0" }
input: { schema: { document: { type: object } } }
do:
  - parent:
      do:
        - child: { set: { x: 1 } }
"#;

    const CASE_B: &str = r#"
document: { dsl: "1.0.0-jobworkerp", namespace: t, name: b, version: "1.0.0" }
input: { schema: { document: { type: object } } }
do:
  - parent:
      if: "${ true }"
      do:
        - child: { set: { x: 1 } }
"#;

    const CASE_C: &str = r#"
document: { dsl: "1.0.0-jobworkerp", namespace: t, name: c, version: "1.0.0" }
input: { schema: { document: { type: object } } }
do:
  - parent:
      if: "${ true }"
      do:
        - inner: { set: { skipped: true } }
      export: { as: { result: "skipped" } }
      then: exit
"#;

    // onError must live at forTask level (sibling of `for`/`do`), not inside `for:`
    const CASE_D_INVALID: &str = r#"
document: { dsl: "1.0.0-jobworkerp", namespace: t, name: d, version: "1.0.0" }
input: { schema: { document: { type: object } } }
do:
  - loop:
      for: { each: x, in: "${ [1,2,3] }", onError: continue }
      do:
        - body: { set: { y: 1 } }
"#;

    const CASE_D_VALID: &str = r#"
document: { dsl: "1.0.0-jobworkerp", namespace: t, name: d2, version: "1.0.0" }
input: { schema: { document: { type: object } } }
do:
  - loop:
      for: { each: x, in: "${ [1,2,3] }" }
      onError: continue
      do:
        - body: { set: { y: 1 } }
"#;

    const CASE_E: &str = r#"
document: { dsl: "1.0.0-jobworkerp", namespace: t, name: e, version: "1.0.0" }
input: { schema: { document: { type: object } } }
do:
  - loop:
      for: { each: x, in: "${ [1,2,3] }" }
      do:
        - body: { set: { y: 1 } }
"#;

    // The schema relies on `allOf:[{$ref: taskBase}, {properties: ...}]` to
    // share base properties across every task type. If this stops propagating
    // evaluated properties through `$ref` in some future jsonschema version,
    // every task type starts rejecting `if`/`then`/`export`/etc. as
    // unevaluated. Detect that regression with a tiny inline schema before
    // anything else fails.
    #[test]
    fn probe_allof_ref_unevaluated_props() {
        let schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$defs": {
                "base": {
                    "type": "object",
                    "properties": {
                        "if": {"type": "string"},
                        "then": {"type": "string"}
                    }
                }
            },
            "type": "object",
            "unevaluatedProperties": false,
            "allOf": [
                {"$ref": "#/$defs/base"},
                {"properties": {"do": {"type": "string"}}}
            ]
        });
        let v = jsonschema::draft202012::new(&schema).unwrap();
        let inst = serde_json::json!({"if": "x", "do": "y", "then": "z"});
        let errs: Vec<_> = v.iter_errors(&inst).map(|e| format!("{}", e)).collect();
        eprintln!("probe errors: {:?}", errs);
        assert!(
            errs.is_empty(),
            "allOf+$ref evaluated-property propagation broken: {errs:?}"
        );
    }

    #[test]
    fn case_a_do_only_passes() {
        run_validate(CASE_A).unwrap();
    }

    #[test]
    fn case_b_do_with_if_passes() {
        run_validate(CASE_B).unwrap();
    }

    // Regression: was previously rejected because $ref + unevaluatedProperties:false
    // marked taskBase props (`if`/`then`/`export`) as unevaluated.
    // Regression: was previously rejected because the validator could not see
    // taskBase properties (`if`/`then`/`export`) through a sibling `$ref` and
    // because `flowDirective` used `oneOf` (where reserved keywords like
    // "exit" also matched the open-ended string branch).
    #[test]
    fn case_c_do_with_if_export_then_passes() {
        run_validate(CASE_C).unwrap();
    }

    #[test]
    fn case_d_invalid_onerror_inside_for_is_rejected() {
        let err = run_validate(CASE_D_INVALID).unwrap_err();
        assert!(
            err.to_string().contains("onError"),
            "expected onError-related rejection, got: {err}"
        );
    }

    #[test]
    fn case_d_valid_onerror_at_for_task_level_passes() {
        run_validate(CASE_D_VALID).unwrap();
    }

    #[test]
    fn case_e_for_without_onerror_passes() {
        run_validate(CASE_E).unwrap();
    }

    // Combined regression: switch + tryTask + raise + flowDirective `exit`
    // — verifies the full set of taskBase-inherited properties on more task
    // types beyond doTask/forTask.
    #[test]
    fn taskbase_props_on_try_switch_raise_pass() {
        let yaml = r#"
document: { dsl: "1.0.0-jobworkerp", namespace: t, name: comb, version: "1.0.0" }
input: { schema: { document: { type: object } } }
do:
  - guard:
      switch:
        - happy:
            when: "${ true }"
            then: continue
        - bail:
            then: exit
  - safeBlock:
      try:
        - inner:
            if: "${ true }"
            do:
              - leaf: { set: { v: 1 } }
            then: continue
      catch:
        as: err
        do:
          - report: { set: { handled: true } }
  - kaboom:
      if: "${ false }"
      raise:
        error:
          type: "https://example.com/errors/test"
          status: 500
"#;
        run_validate(yaml).unwrap();
    }
}
