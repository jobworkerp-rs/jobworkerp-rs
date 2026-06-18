use crate::workflow::definition::{transform::UseExpressionTransformer, workflow};
use crate::workflow::execute::context::TaskContext;
use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NormalizedRun {
    /// Workflow position segment for this alias ("shell" / "container" /
    /// "workflow"). Single source of truth for the segment name so call sites
    /// don't re-derive it from the `RunTaskConfiguration` variant.
    pub(crate) position_name: &'static str,
    pub(crate) runner_name: &'static str,
    pub(crate) arguments: Value,
    pub(crate) using: Option<String>,
}

/// Failure modes when turning a run alias into a [`NormalizedRun`]. Kept
/// distinct so call sites can map them onto the right workflow error: transform
/// failures already carry a positioned `workflow::Error`, everything else is a
/// bad-argument detail string.
#[derive(Debug)]
pub(crate) enum AliasError {
    BadArgument { message: String, detail: String },
    Transform(Box<workflow::Error>),
}

/// Serialize a run alias, expand its expressions, and normalize it into the
/// runner invocation it stands for. Shared by the streaming and non-streaming
/// run executors so both interpret aliases identically.
pub(crate) fn interpret_run_alias<E: UseExpressionTransformer>(
    run: &workflow::RunTaskConfiguration,
    raw_input: Arc<Value>,
    expression: &BTreeMap<String, Arc<Value>>,
) -> Result<NormalizedRun, AliasError> {
    let alias_value = serde_json::to_value(run).map_err(|e| AliasError::BadArgument {
        message: "Failed to serialize run alias".to_string(),
        detail: e.to_string(),
    })?;
    let alias_map = match alias_value {
        Value::Object(map) => map,
        other => {
            return Err(AliasError::BadArgument {
                message: "Run alias must serialize to an object".to_string(),
                detail: format!("Invalid run alias: {other:#?}"),
            });
        }
    };
    let (alias_map, preserved_workflow_data) = preserve_workflow_data(alias_map);
    let mut transformed =
        E::transform_map(raw_input, alias_map, expression).map_err(AliasError::Transform)?;
    restore_workflow_data(&mut transformed, preserved_workflow_data);
    normalize_alias_value(transformed).map_err(|e| AliasError::BadArgument {
        message: "Failed to normalize run alias".to_string(),
        detail: e.to_string(),
    })
}

fn preserve_workflow_data(
    mut alias_map: Map<String, Value>,
) -> (Map<String, Value>, Option<Value>) {
    let preserved = alias_map
        .get_mut("workflow")
        .and_then(Value::as_object_mut)
        .and_then(|workflow| workflow.remove("workflowData"));
    (alias_map, preserved)
}

fn restore_workflow_data(transformed: &mut Value, workflow_data: Option<Value>) {
    let Some(workflow_data) = workflow_data else {
        return;
    };
    let Some(workflow) = transformed
        .as_object_mut()
        .and_then(|alias| alias.get_mut("workflow"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    workflow.insert("workflowData".to_string(), workflow_data);
}

/// Resolve a run alias against the live task context and return a runner
/// invocation ready to dispatch. Centralizes the `AliasError` → positioned
/// `workflow::Error` mapping and the inherited-context attachment so the
/// streaming and non-streaming executors share one prelude and only differ in
/// how they finally dispatch the [`NormalizedRun`]. On success the caller is
/// left with the alias position segment already pushed.
pub(crate) async fn resolve_run_alias<E: UseExpressionTransformer>(
    run: &workflow::RunTaskConfiguration,
    task_context: &TaskContext,
    expression: &BTreeMap<String, Arc<Value>>,
) -> Result<NormalizedRun, Box<workflow::Error>> {
    let factory = workflow::errors::ErrorFactory::new;
    let mut normalized = match interpret_run_alias::<E>(run, task_context.input.clone(), expression)
    {
        Ok(normalized) => normalized,
        Err(AliasError::Transform(mut e)) => {
            e.position(&*task_context.position.read().await);
            return Err(e);
        }
        Err(AliasError::BadArgument { message, detail }) => {
            let pos = task_context.position.read().await.as_error_instance();
            return Err(factory().bad_argument(message, Some(pos), Some(detail)));
        }
    };
    // Only workflow aliases inherit the parent context, so avoid cloning the
    // (potentially large) context map for shell/container aliases.
    let inherit_result = if normalized.position_name == "workflow" {
        let context_variables = task_context.context_variables.lock().await.clone();
        attach_inherited_workflow_context(&mut normalized, context_variables)
    } else {
        Ok(())
    };
    if let Err(e) = inherit_result {
        let pos = task_context.position.read().await.as_error_instance();
        return Err(factory().bad_argument(
            "Failed to inherit workflow context".to_string(),
            Some(pos),
            Some(e.to_string()),
        ));
    }
    task_context
        .add_position_name(normalized.position_name.to_string())
        .await;
    Ok(normalized)
}

pub(crate) fn normalize_alias_value(value: Value) -> Result<NormalizedRun> {
    let Value::Object(mut obj) = value else {
        return Err(anyhow!("run alias must be an object"));
    };

    match (
        obj.remove("shell"),
        obj.remove("container"),
        obj.remove("workflow"),
    ) {
        (Some(shell), None, None) => normalize_shell(shell),
        (None, Some(container), None) => normalize_container(container),
        (None, None, Some(workflow)) => normalize_workflow(workflow),
        _ => Err(anyhow!(
            "run alias must contain exactly one of shell, container, or workflow"
        )),
    }
}

fn normalize_shell(shell: Value) -> Result<NormalizedRun> {
    let arguments = match shell {
        Value::String(command) => json!({ "command": command }),
        Value::Object(shell) => Value::Object(shell),
        other => {
            return Err(anyhow!(
                "run.shell must be a string or object, got {}",
                type_name(&other)
            ));
        }
    };

    Ok(NormalizedRun {
        position_name: "shell",
        runner_name: "COMMAND",
        arguments,
        using: None,
    })
}

fn normalize_container(container: Value) -> Result<NormalizedRun> {
    let Value::Object(mut container) = container else {
        return Err(anyhow!("run.container must be an object"));
    };

    if !container.contains_key("image") {
        return Err(anyhow!("run.container.image is required"));
    }

    // The only fields that differ from the DOCKER runner schema: `env` is a
    // map here but a `KEY=VALUE` array there, and `command` is named `cmd`.
    // Every other key passes through unchanged.
    if let Some(env) = container.remove("env") {
        container.insert("env".to_string(), Value::Array(env_to_vec(env)?));
    }
    if let Some(command) = container.remove("command") {
        container.insert("cmd".to_string(), command);
    }

    Ok(NormalizedRun {
        position_name: "container",
        runner_name: "DOCKER",
        arguments: Value::Object(container),
        using: None,
    })
}

fn normalize_workflow(workflow: Value) -> Result<NormalizedRun> {
    let Value::Object(mut workflow) = workflow else {
        return Err(anyhow!("run.workflow must be an object"));
    };

    if !workflow.contains_key("workflowData") && !workflow.contains_key("workflowUrl") {
        return Err(anyhow!("run.workflow requires workflowData or workflowUrl"));
    }
    if workflow.contains_key("workflowContext") {
        return Err(anyhow!(
            "run.workflow.workflowContext is not a DSL field; parent workflow context is inherited implicitly"
        ));
    }
    // The WORKFLOW runner expects `input` as a JSON string.
    if let Some(input) = workflow.remove("input") {
        workflow.insert("input".to_string(), value_to_string(input)?);
    } else {
        workflow.insert("input".to_string(), Value::String("{}".to_string()));
    }

    Ok(NormalizedRun {
        position_name: "workflow",
        runner_name: "WORKFLOW",
        arguments: Value::Object(workflow),
        using: Some("run".to_string()),
    })
}

pub(crate) fn attach_inherited_workflow_context(
    normalized: &mut NormalizedRun,
    context_variables: Map<String, Value>,
) -> Result<()> {
    if normalized.position_name != "workflow" {
        return Ok(());
    }

    let workflow_context = serde_json::to_string(&Value::Object(context_variables))
        .map_err(|e| anyhow!("failed to serialize inherited workflow context: {e}"))?;
    let Value::Object(args) = &mut normalized.arguments else {
        return Err(anyhow!("normalized run arguments must be an object"));
    };
    args.insert(
        "workflowContext".to_string(),
        Value::String(workflow_context),
    );
    Ok(())
}

fn env_to_vec(env: Value) -> Result<Vec<Value>> {
    let env = env
        .as_object()
        .ok_or_else(|| anyhow!("run.container.env must be an object"))?;
    env.iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| Value::String(format!("{key}={value}")))
                .ok_or_else(|| anyhow!("run.container.env values must be strings"))
        })
        .collect()
}

fn value_to_string(value: Value) -> Result<Value> {
    match value {
        Value::String(s) => Ok(Value::String(s)),
        other => serde_json::to_string(&other)
            .map(Value::String)
            .map_err(|e| anyhow!("failed to serialize workflow value: {e}")),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::workflow::{
        RunShell, RunTaskConfiguration, RunWorkflow, ShellConfiguration, WorkflowConfiguration,
    };

    #[test]
    fn shell_string_maps_to_command_runner_arguments() {
        let normalized = normalize_alias_value(json!({ "shell": "echo hello" })).unwrap();

        assert_eq!(normalized.position_name, "shell");
        assert_eq!(normalized.runner_name, "COMMAND");
        assert_eq!(normalized.using, None);
        assert_eq!(normalized.arguments, json!({ "command": "echo hello" }));
    }

    #[test]
    fn shell_object_preserves_command_options() {
        let normalized = normalize_alias_value(json!({
            "shell": {
                "command": "grep",
                "args": ["needle", "file.txt"],
                "workingDir": "/work",
                "withMemoryMonitoring": true,
                "treatNonzeroAsError": true,
                "successExitCodes": [0, 1]
            }
        }))
        .unwrap();

        assert_eq!(normalized.runner_name, "COMMAND");
        assert_eq!(
            normalized.arguments,
            json!({
                "command": "grep",
                "args": ["needle", "file.txt"],
                "workingDir": "/work",
                "withMemoryMonitoring": true,
                "treatNonzeroAsError": true,
                "successExitCodes": [0, 1]
            })
        );
    }

    #[test]
    fn container_maps_to_docker_runner_arguments() {
        let normalized = normalize_alias_value(json!({
            "container": {
                "image": "alpine:3.20",
                "command": ["echo", "hello"],
                "entrypoint": ["/bin/sh", "-c"],
                "env": {"GREETING": "hello"},
                "workingDir": "/work",
                "timeoutSec": 30,
                "treatNonzeroAsError": true,
                "successExitCodes": [0]
            }
        }))
        .unwrap();

        let env = normalized
            .arguments
            .get("env")
            .and_then(Value::as_array)
            .cloned()
            .unwrap();

        assert_eq!(normalized.position_name, "container");
        assert_eq!(normalized.runner_name, "DOCKER");
        assert_eq!(normalized.arguments["image"], "alpine:3.20");
        assert_eq!(normalized.arguments["cmd"], json!(["echo", "hello"]));
        assert_eq!(normalized.arguments["entrypoint"], json!(["/bin/sh", "-c"]));
        assert_eq!(env, vec![json!("GREETING=hello")]);
        assert_eq!(normalized.arguments["workingDir"], "/work");
        assert_eq!(normalized.arguments["timeoutSec"], 30);
        assert_eq!(normalized.arguments["treatNonzeroAsError"], true);
        assert_eq!(normalized.arguments["successExitCodes"], json!([0]));
    }

    #[test]
    fn workflow_maps_to_workflow_runner_with_json_string_input() {
        let normalized = normalize_alias_value(json!({
            "workflow": {
                "workflowData": "document: { dsl: \"1.0.0-jobworkerp\", namespace: t, name: child, version: \"1.0.0\" }\ndo: []",
                "input": {"value": 1},
                "executionId": "child-1"
            }
        }))
        .unwrap();

        assert_eq!(normalized.position_name, "workflow");
        assert_eq!(normalized.runner_name, "WORKFLOW");
        assert_eq!(normalized.using, Some("run".to_string()));
        assert_eq!(normalized.arguments["input"], r#"{"value":1}"#);
        assert!(normalized.arguments.get("workflowContext").is_none());
        assert_eq!(normalized.arguments["executionId"], "child-1");
    }

    #[test]
    fn workflow_context_argument_is_rejected() {
        let err = normalize_alias_value(json!({
            "workflow": {
                "workflowData": "document: { dsl: \"1.0.0-jobworkerp\", namespace: t, name: child, version: \"1.0.0\" }\ndo: []",
                "workflowContext": {"trace": "yes"}
            }
        }))
        .unwrap_err();

        assert!(err.to_string().contains("inherited"));
    }

    #[test]
    fn workflow_inherits_parent_context_variables() {
        let mut normalized = normalize_alias_value(json!({
            "workflow": {
                "workflowData": "document: { dsl: \"1.0.0-jobworkerp\", namespace: t, name: child, version: \"1.0.0\" }\ndo: []"
            }
        }))
        .unwrap();
        let context_variables = Map::from_iter([
            ("trace".to_string(), json!("yes")),
            ("count".to_string(), json!(1)),
        ]);

        attach_inherited_workflow_context(&mut normalized, context_variables).unwrap();

        assert_eq!(
            serde_json::from_str::<Value>(
                normalized.arguments["workflowContext"].as_str().unwrap()
            )
            .unwrap(),
            json!({"count": 1, "trace": "yes"})
        );
    }

    #[test]
    fn workflow_data_without_input_sets_empty_input() {
        let normalized = normalize_alias_value(json!({
            "workflow": {
                "workflowData": "document: { dsl: \"1.0.0-jobworkerp\", namespace: t, name: child, version: \"1.0.0\" }\ndo: []"
            }
        }))
        .unwrap();

        assert_eq!(
            normalized.arguments["workflowData"],
            "document: { dsl: \"1.0.0-jobworkerp\", namespace: t, name: child, version: \"1.0.0\" }\ndo: []"
        );
        assert_eq!(normalized.arguments["input"], "{}");
    }

    #[test]
    fn workflow_url_without_input_sets_empty_input() {
        let normalized = normalize_alias_value(json!({
            "workflow": {
                "workflowUrl": "file:///tmp/child.yaml"
            }
        }))
        .unwrap();

        assert_eq!(
            normalized.arguments["workflowUrl"],
            "file:///tmp/child.yaml"
        );
        assert_eq!(normalized.arguments["input"], "{}");
    }

    #[test]
    fn ambiguous_alias_is_rejected() {
        let err = normalize_alias_value(json!({
            "shell": "echo hello",
            "container": {"image": "alpine"}
        }))
        .unwrap_err();

        assert!(err.to_string().contains("exactly one"));
    }

    #[test]
    fn generated_shell_variant_serializes_to_normalizable_alias() {
        let run = RunTaskConfiguration::Shell(RunShell {
            shell: ShellConfiguration::Variant0("echo hello".to_string()),
        });

        let normalized = normalize_alias_value(serde_json::to_value(run).unwrap()).unwrap();

        assert_eq!(normalized.runner_name, "COMMAND");
        assert_eq!(normalized.arguments, json!({ "command": "echo hello" }));
    }

    /// Minimal transformer so `interpret_run_alias` can be exercised without a
    /// full executor; relies entirely on the trait's default `transform_map`.
    struct TestTransformer;
    impl crate::workflow::definition::transform::UseJqAndTemplateTransformer for TestTransformer {}
    impl UseExpressionTransformer for TestTransformer {}

    #[test]
    fn interpret_run_alias_expands_expressions_then_normalizes() {
        let run = RunTaskConfiguration::Shell(RunShell {
            shell: ShellConfiguration::Variant0("${ .cmd }".to_string()),
        });
        let raw_input = Arc::new(json!({ "cmd": "echo hi" }));

        let normalized =
            interpret_run_alias::<TestTransformer>(&run, raw_input, &BTreeMap::new()).unwrap();

        assert_eq!(normalized.runner_name, "COMMAND");
        // The jq expression resolved against raw_input before normalization.
        assert_eq!(normalized.arguments, json!({ "command": "echo hi" }));
    }

    #[test]
    fn interpret_run_alias_preserves_workflow_data_child_expressions() {
        let child_workflow = r#"
document: { dsl: "1.0.0-jobworkerp", namespace: t, name: child, version: "1.0.0" }
do:
  - use-child-input:
      set:
        value: ${ .child_value }
"#;
        let run = RunTaskConfiguration::Workflow(RunWorkflow {
            workflow: WorkflowConfiguration::Variant0 {
                execution_id: Some("${ .execution_id }".to_string()),
                input: Some(json!({ "child_value": "${ .parent_value }" })),
                workflow_data: child_workflow.to_string(),
            },
        });
        let raw_input = Arc::new(json!({
            "execution_id": "child-1",
            "parent_value": "from-parent"
        }));

        let normalized =
            interpret_run_alias::<TestTransformer>(&run, raw_input, &BTreeMap::new()).unwrap();

        assert_eq!(normalized.runner_name, "WORKFLOW");
        assert_eq!(normalized.arguments["workflowData"], child_workflow);
        assert_eq!(normalized.arguments["executionId"], "child-1");
        assert_eq!(
            normalized.arguments["input"],
            r#"{"child_value":"from-parent"}"#
        );
    }

    #[test]
    fn interpret_run_alias_surfaces_transform_errors() {
        let run = RunTaskConfiguration::Shell(RunShell {
            shell: ShellConfiguration::Variant0("${ . | invalid_jq_func }".to_string()),
        });

        let err =
            interpret_run_alias::<TestTransformer>(&run, Arc::new(json!({})), &BTreeMap::new())
                .unwrap_err();

        assert!(matches!(err, AliasError::Transform(_)));
    }
}
