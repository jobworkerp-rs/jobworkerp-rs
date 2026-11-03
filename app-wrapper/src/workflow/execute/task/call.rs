//! `call` task executor.
//!
//! Dispatches an Open Workflow `call` task to the matching protocol
//! adapter. The HTTP-specific logic lives in [`http`] and the gRPC-specific
//! logic in [`grpc`]; both are adapter conversions onto existing built-in
//! runners (HTTP_REQUEST / GRPC). Shared concerns — expression/secret
//! resolution, job enqueue, and the `call`/`with` dispatch — stay here.

pub mod grpc;
pub mod http;

use super::TaskExecutorTrait;
use crate::workflow::execute::expression::ExpressionContext;
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow,
    },
    execute::{
        context::{ChildJobScope, TaskContext, WorkflowContext},
        expression::UseExpression,
        secret,
        task::{
            NamedTimeouts,
            child_job::{self, ChildJobStage},
            run::resolve_run_task_timeout_sec,
        },
    },
};
use anyhow::{Context, Result, anyhow};
use app::app::job::execute::{JobExecutorWrapper, UseJobExecutor, WorkerForEnqueue};
use command_utils::trace::Tracing;
use infra::workflow::definition::expression_context::SECRETS_VARIABLE;
use proto::jobworkerp::data::{QueueType, ResponseType, StreamingType, WorkerData};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;

// Upper bound on the downstream response text copied into an error `detail`,
// so an oversized error body cannot bloat the stored result or the client
// stream.
const ERROR_DETAIL_MAX_BYTES: usize = 64 * 1024;
const TRUNCATED_MARKER: &str = "... (truncated)";

pub struct CallTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Option<Duration>,
    named_timeouts: Arc<NamedTimeouts>,
    task: workflow::CallTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
}

impl UseExpression for CallTaskExecutor {}
impl UseJqAndTemplateTransformer for CallTaskExecutor {}
impl UseExpressionTransformer for CallTaskExecutor {}
impl Tracing for CallTaskExecutor {}

impl CallTaskExecutor {
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_task_timeout: Option<Duration>,
        named_timeouts: Arc<NamedTimeouts>,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        task: workflow::CallTask,
        metadata: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            workflow_context,
            default_task_timeout,
            named_timeouts,
            task,
            job_executor_wrapper,
            metadata,
        }
    }

    // Protocol-agnostic: `field` (e.g. "authentication", "endpoint", "with")
    // already identifies the offending part, and the dispatched call type is
    // recorded in the task position, so the message stays accurate for both
    // `call: http` and `call: grpc`.
    fn unsupported(field: &str, message: String) -> Box<workflow::Error> {
        workflow::errors::ErrorFactory::new().configuration(
            format!("Unsupported call {field}: {message}"),
            None,
            None,
        )
    }

    /// Secrets the workflow cannot read are authorization errors, as the
    /// specification requires; malformed secret values or unsupported schemes
    /// are definition problems and stay configuration errors.
    fn secret_failure(field: &str, error: anyhow::Error) -> Box<workflow::Error> {
        if error.downcast_ref::<secret::SecretAccessError>().is_some() {
            workflow::errors::ErrorFactory::new().authorization(
                format!("Inaccessible secret in call {field}: {error}"),
                None,
                None,
            )
        } else {
            Self::unsupported(field, error.to_string())
        }
    }

    /// Downstream text (a response body or status message) for an error
    /// `detail`. Secrets are masked before truncation so that the size limit
    /// cannot cut a secret and leave its head exposed; the service may echo
    /// request credentials back in its error text.
    fn error_detail(text: &str, secret_values: &[String]) -> String {
        let masked = secret::mask_encoded_secret_values(text, secret_values);
        Self::truncate_error_detail(masked.as_deref().unwrap_or(text))
    }

    fn truncate_error_detail(detail: &str) -> String {
        if detail.len() <= ERROR_DETAIL_MAX_BYTES {
            return detail.to_string();
        }
        let end = detail.floor_char_boundary(ERROR_DETAIL_MAX_BYTES);
        format!("{}{TRUNCATED_MARKER}", &detail[..end])
    }

    fn merge_task_metadata(
        workflow_metadata: &HashMap<String, String>,
        task_metadata: &serde_json::Map<String, Value>,
    ) -> HashMap<String, String> {
        let mut metadata = workflow_metadata.clone();
        for (key, value) in task_metadata {
            if !metadata.contains_key(key)
                && let Some(value) = value.as_str()
            {
                metadata.insert(key.clone(), value.to_string());
            }
        }
        metadata
    }

    /// Resolve a call authentication into a concrete policy, shared by the HTTP
    /// endpoint and gRPC service adapters.
    ///
    /// Their `with` authentication is the same shape — either an inline policy
    /// or a `{ use: <name> }` reference — but lives under protocol-specific enum
    /// types, so each adapter destructures its enum into `(inline, use_ref)` and
    /// delegates here.
    ///
    /// An inline policy is already expanded by the caller's `with` transform and
    /// returned as-is. A named (`use:`) policy is stored verbatim in
    /// `use.authentications` and expanded here with the same expression context;
    /// its own `$secrets` references are resolved once the `use` name is known
    /// (the name itself may have been expression-driven). Only the secrets that
    /// policy references are loaded, into a local expression copy — never the
    /// shared context. Their values are returned with the policy so that the
    /// caller can mask them (an inline policy's secrets are already among the
    /// `with` secrets).
    fn resolve_named_policy(
        inline: Option<&workflow::AuthenticationPolicy>,
        use_ref: Option<&str>,
        named: &HashMap<String, workflow::AuthenticationPolicy>,
        declared_secrets: &std::collections::HashSet<String>,
        raw_input: Arc<Value>,
        expression: &ExpressionContext,
    ) -> Result<(workflow::AuthenticationPolicy, Vec<String>), Box<workflow::Error>> {
        if let Some(policy) = inline {
            return Ok((policy.clone(), Vec::new()));
        }
        let use_ref = use_ref.ok_or_else(|| {
            Self::unsupported(
                "authentication",
                "authentication must be inline or a use reference".to_string(),
            )
        })?;
        let policy = named.get(use_ref).ok_or_else(|| {
            Self::unsupported(
                "authentication",
                format!("authentication '{use_ref}' is not defined in use.authentications"),
            )
        })?;
        let policy_value = serde_json::to_value(policy).map_err(|e| {
            Self::unsupported(
                "authentication",
                format!("failed to serialize authentication '{use_ref}': {e}"),
            )
        })?;
        let policy_secrets =
            secret::resolve_secrets_for(declared_secrets, &policy_value.to_string())
                .map_err(|e| Self::secret_failure("authentication", e))?;
        let policy_secret_values = secret::secret_values(&policy_secrets);
        let mut policy_expression = expression.clone();
        secret::merge_secrets(&mut policy_expression, policy_secrets);
        let transformed = Self::transform_value(raw_input, policy_value, &policy_expression)?;
        let policy = serde_json::from_value(transformed).map_err(|e| {
            Self::unsupported(
                "authentication",
                format!("invalid authentication '{use_ref}' after expansion: {e}"),
            )
        })?;
        Ok((policy, policy_secret_values))
    }

    /// The communication error for a call whose runner job failed, with secrets
    /// masked. Shared by both protocols so that no failure path skips masking.
    fn runner_failure(
        error: anyhow::Error,
        title: &str,
        position: String,
        secret_values: &[String],
    ) -> Box<workflow::Error> {
        let error = child_job::to_workflow_error(
            error,
            workflow::errors::StandardErrorType::Communication,
            title,
            Some(position),
        );
        Self::mask_error(error, secret_values)
    }

    /// `error` with secrets masked in its title and detail. A failed child job
    /// is reported with its full error chain, which can carry request data —
    /// e.g. a URL with secret query values, or a server error echoing the
    /// credentials it received.
    fn mask_error(
        mut error: Box<workflow::Error>,
        secret_values: &[String],
    ) -> Box<workflow::Error> {
        for text in [&mut error.title, &mut error.detail].into_iter().flatten() {
            if let Some(masked) = secret::mask_encoded_secret_values(text, secret_values) {
                *text = masked;
            }
        }
        error
    }

    // Enqueue a job on the given built-in runner and return its transformed raw
    // output. `using` selects the runner method (HTTP_REQUEST is single-method
    // so it passes None; GRPC must pass Some("unary") so args/result descriptor
    // resolution and the worker-side `resolve_method` pick the unary method).
    #[allow(clippy::too_many_arguments)]
    async fn execute_runner(
        &self,
        cx: Arc<opentelemetry::Context>,
        runner_name: &str,
        using: Option<&str>,
        settings: Value,
        job_args: Value,
        worker_name: &str,
        timeout_sec: u32,
        child_job_scope: Option<&Arc<ChildJobScope>>,
    ) -> Result<Value> {
        let runner = child_job::find_runner(&self.job_executor_wrapper, runner_name).await?;
        let settings = self
            .job_executor_wrapper
            .setup_runner_and_settings(&runner, Some(settings))
            .await
            .context(ChildJobStage::Settings)?;
        let (rid, rdata) = runner
            .id
            .zip(runner.data)
            .ok_or_else(|| anyhow!("Runner '{runner_name}' missing id/data"))?;
        let job_args = self
            .job_executor_wrapper
            .transform_job_args(&rid, &rdata, &job_args, using)
            .await
            .context(ChildJobStage::Prepare)?;

        let mut metadata = Self::merge_task_metadata(&self.metadata, &self.task.metadata);
        Self::inject_metadata_from_context(&mut metadata, &cx);
        let worker_data = WorkerData {
            name: worker_name.to_string(),
            description: "Open Workflow call task temporary worker".to_string(),
            runner_id: Some(rid),
            runner_settings: settings,
            queue_type: QueueType::Normal as i32,
            response_type: ResponseType::Direct as i32,
            store_failure: true,
            broadcast_results: true,
            ..Default::default()
        };
        let (result_fut, running_job_guard) = super::enqueue_child_job(
            &self.workflow_context,
            &self.job_executor_wrapper,
            child_job_scope,
            self.job_executor_wrapper
                .enqueue_with_worker_or_temp_channel(
                    Arc::new(metadata),
                    WorkerForEnqueue::Temp(worker_data),
                    job_args,
                    None,
                    timeout_sec,
                    StreamingType::None,
                    using.map(|u| u.to_string()),
                    None,
                ),
            super::ChildJobTracking::Tracked,
        )
        .await
        .context(ChildJobStage::Enqueue)?;
        let wait_result = result_fut.await;
        running_job_guard
            .expect("tracked child jobs must return a running job guard")
            .mark_completed();

        let output = child_job::finished_output(&self.job_executor_wrapper, wait_result)?;
        self.job_executor_wrapper
            .transform_raw_output(&rid, &rdata, output.as_slice(), using)
            .await
            .context(ChildJobStage::Decode)
    }
}

impl TaskExecutorTrait<'_> for CallTaskExecutor {
    async fn execute(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: &str,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        task_context.add_position_name("call".to_string()).await;

        // Snapshot the workflow's reusable auth/secret declarations and build the
        // expression context under a single read guard. The declarations are
        // serde(skip) on WorkflowContext, so they are never serialized into the
        // expression context below.
        let (named_authentications, declared_secrets, mut expression) = {
            let wfc = self.workflow_context.read().await;
            let named_authentications = wfc.authentications.clone();
            let declared_secrets = wfc.declared_secrets.clone();
            let expression = Self::expression(&wfc, Arc::new(task_context.clone())).await?;
            (named_authentications, declared_secrets, expression)
        };

        let call_value = serde_json::to_value(&self.task.with)
            .map_err(|e| Self::unsupported("with", format!("serialization failed: {e}")))?;

        // Inject the secrets that `with` itself references as `$secrets.<name>`
        // for this call task only. Only secrets actually referenced are resolved
        // (an unused declaration must not force its env var to be set). Secrets
        // referenced *inside a named authentication policy* are resolved later in
        // `resolve_authentication`, once the `use` name is known (it may itself be
        // expression-driven). Resolved from env; never written back to any shared
        // context.
        // Undeclared `$secrets.X` references must error before transformation,
        // even when no secrets are declared at all (otherwise jq yields `null`
        // and Liquid an empty string, e.g. forging an empty `Bearer ` header).
        // `resolve_secrets_for` scans `with` once for both the undeclared-guard
        // and the resolution; the empty-declarations case still runs the guard.
        let secrets = secret::resolve_secrets_for(&declared_secrets, &call_value.to_string())
            .map_err(|e| Self::secret_failure("authentication", e))?;
        let secret_values = secret::secret_values(&secrets);
        if !declared_secrets.is_empty() {
            expression.insert(SECRETS_VARIABLE, Arc::new(secrets));
        }

        let transformed_call =
            match Self::transform_value(task_context.input.clone(), call_value, &expression) {
                Ok(value) => value,
                Err(mut e) => {
                    task_context.add_position_name("with".to_string()).await;
                    let pos = task_context.position.read().await;
                    e.position(&pos);
                    return Err(e);
                }
            };
        let with: workflow::CallTaskWith = serde_json::from_value(transformed_call)
            .map_err(|e| Self::unsupported("with", format!("invalid transformed call: {e}")))?;
        let timeout_sec = resolve_run_task_timeout_sec(
            self.task.timeout.as_ref(),
            &self.named_timeouts,
            self.default_task_timeout,
        )?;

        // Guard against a `call`/`with` mismatch. The `with` oneOf is untagged,
        // so an HTTP-shaped payload under `call: grpc` (or vice versa) would be
        // silently misrouted by serde's first-match. Reject the mismatch
        // explicitly instead of executing the wrong protocol.
        let raw_output = match (&self.task.call, with) {
            (workflow::CallTaskType::Http, workflow::CallTaskWith::Http(call)) => {
                self.execute_http(
                    cx,
                    task_name,
                    &mut task_context,
                    call,
                    &named_authentications,
                    &declared_secrets,
                    &secret_values,
                    &expression,
                    timeout_sec,
                )
                .await?
            }
            (workflow::CallTaskType::Grpc, workflow::CallTaskWith::Grpc(call)) => {
                self.execute_grpc(
                    cx,
                    task_name,
                    &mut task_context,
                    call,
                    &named_authentications,
                    &declared_secrets,
                    &secret_values,
                    &expression,
                    timeout_sec,
                )
                .await?
            }
            (call_type, _) => {
                return Err(Self::unsupported(
                    "with",
                    format!("`with` payload does not match call type '{call_type}'"),
                ));
            }
        };
        task_context.set_raw_output(raw_output);
        task_context.remove_position().await;
        Ok(task_context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Secret-scan tests live here because they exercise the `secret` module's
    // behavior in the `call` task context (the executor resolves `$secrets`
    // before dispatching to either protocol adapter).

    fn no_secrets() -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    fn declared(names: &[&str]) -> std::collections::HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn assert_kind(error: &workflow::Error, kind: workflow::errors::StandardErrorType) {
        assert_eq!(error.type_.0, kind.uri(), "{error:?}");
        assert_eq!(error.status, kind.default_status(), "{error:?}");
    }

    #[test]
    fn undeclared_secret_reference_is_an_authorization_error() {
        let err = secret::resolve_secrets_for(&no_secrets(), r#"{"t":"${ $secrets.MISSING }"}"#)
            .unwrap_err();
        let error = CallTaskExecutor::secret_failure("authentication", err);
        assert_kind(&error, workflow::errors::StandardErrorType::Authorization);
    }

    #[test]
    fn unset_declared_secret_is_an_authorization_error() {
        let err =
            secret::resolve_secret("API_TOKEN", &declared(&["API_TOKEN"]), |_| None).unwrap_err();
        let error = CallTaskExecutor::secret_failure("authentication", err);
        assert_kind(&error, workflow::errors::StandardErrorType::Authorization);
    }

    #[test]
    fn malformed_secret_value_is_a_configuration_error_without_the_value() {
        let err =
            secret::interpret_bearer_secret("API_TOKEN", "{not-json SECRET-VALUE").unwrap_err();
        let error = CallTaskExecutor::secret_failure("authentication", err);
        assert_kind(&error, workflow::errors::StandardErrorType::Configuration);
        assert!(!format!("{error:?}").contains("SECRET-VALUE"), "{error:?}");
    }

    #[test]
    fn unsupported_call_settings_are_configuration_errors() {
        let error = CallTaskExecutor::unsupported("endpoint", "bad".to_string());
        assert_kind(&error, workflow::errors::StandardErrorType::Configuration);
    }

    #[test]
    fn referenced_secret_names_match_jq_and_liquid_forms() {
        let names = secret::referenced_secret_names(r#"{"t":"${ $secrets.API_TOKEN }"}"#);
        assert!(names.contains("API_TOKEN"));
        let names = secret::referenced_secret_names(r#"{"t":"$${{{ secrets.API_TOKEN }}}"}"#);
        assert!(names.contains("API_TOKEN"));
        // Plain text with no expression span yields nothing.
        assert!(secret::referenced_secret_names(r#"{"t":"no reference here"}"#).is_empty());
        // A different secret name is not reported for an unrelated reference.
        let names = secret::referenced_secret_names(r#"{"t":"${ $secrets.OTHER }"}"#);
        assert!(!names.contains("API_TOKEN"));
        assert!(names.contains("OTHER"));
    }

    #[test]
    fn referenced_secret_names_match_bracket_notation() {
        // Names that are not valid bare jq identifiers must use bracket notation.
        // As serialized by serde_json, the inner double quotes are escaped.
        let serialized =
            serde_json::json!({"token": "${ $secrets[\"github-token\"] }"}).to_string();
        assert!(secret::referenced_secret_names(&serialized).contains("github-token"));
        // Single-quote bracket form (not escaped in JSON).
        let names = secret::referenced_secret_names(r#"{"t":"${ $secrets['github-token'] }"}"#);
        assert!(names.contains("github-token"));
        // Unrelated name is not reported.
        assert!(!secret::referenced_secret_names(&serialized).contains("other-token"));
    }

    #[test]
    fn referenced_secret_names_respect_dot_notation_boundary() {
        // `${ $secrets.API_TOKEN }` references `API_TOKEN`, not the prefix `API`.
        let names = secret::referenced_secret_names(r#"{"t":"${ $secrets.API_TOKEN }"}"#);
        assert!(names.contains("API_TOKEN"));
        assert!(!names.contains("API"));
        // But an exact dot reference to the shorter name is matched.
        let names = secret::referenced_secret_names(r#"{"t":"${ $secrets.API }"}"#);
        assert!(names.contains("API"));
    }

    #[test]
    fn referenced_secret_names_extracts_dot_and_bracket_forms() {
        let serialized = serde_json::json!({
            "a": "${ $secrets.API_TOKEN }",
            "b": "$${{{ secrets.OTHER }}}",
            "c": "${ $secrets[\"github-token\"] }",
            "d": "${ $secrets['gitlab-token'] }",
        })
        .to_string();
        let names = secret::referenced_secret_names(&serialized);
        assert!(names.contains("API_TOKEN"));
        assert!(names.contains("OTHER"));
        assert!(names.contains("github-token"));
        assert!(names.contains("gitlab-token"));
    }

    #[test]
    fn referenced_secret_names_ignores_text_without_references() {
        let names = secret::referenced_secret_names(r#"{"t":"no secrets used"}"#);
        assert!(names.is_empty());
    }

    #[test]
    fn undeclared_secret_reference_is_rejected() {
        // Declared only TOKEN, but the expression references MISSING.
        let serialized = r#"{"t":"$${{{ secrets.MISSING }}}"}"#;
        let err = secret::ensure_no_undeclared_secret_references(&declared(&["TOKEN"]), serialized)
            .unwrap_err();
        assert!(err.to_string().contains("MISSING"));
        assert!(err.to_string().contains("not declared"));
    }

    #[test]
    fn undeclared_secret_reference_with_no_declarations_is_rejected() {
        // The empty-declaration case must still error (not silently expand).
        let serialized = r#"{"t":"${ $secrets.MISSING }"}"#;
        let err =
            secret::ensure_no_undeclared_secret_references(&no_secrets(), serialized).unwrap_err();
        assert!(err.to_string().contains("MISSING"));
    }

    #[test]
    fn declared_secret_reference_is_accepted() {
        let serialized = r#"{"t":"${ $secrets.API_TOKEN }"}"#;
        assert!(
            secret::ensure_no_undeclared_secret_references(&declared(&["API_TOKEN"]), serialized,)
                .is_ok()
        );
    }

    #[test]
    fn undeclared_bracket_secret_reference_is_rejected() {
        let serialized = serde_json::json!({"t": "${ $secrets[\"github-token\"] }"}).to_string();
        let err =
            secret::ensure_no_undeclared_secret_references(&declared(&["other"]), &serialized)
                .unwrap_err();
        assert!(err.to_string().contains("github-token"));
    }

    #[test]
    fn literal_secrets_text_outside_expression_is_not_a_reference() {
        // A URI or body that merely contains the substring `secrets.<name>` must
        // not be treated as a secret reference: only `${...}` / `$${...}` spans
        // count. Otherwise an ordinary call.http would spuriously fail.
        let serialized = serde_json::json!({
            "endpoint": {"uri": "https://example.com/secrets.API_TOKEN"},
            "body": "see secrets.MISSING for details",
        })
        .to_string();
        assert!(
            secret::referenced_secret_names(&serialized).is_empty(),
            "literal text must not yield secret references"
        );
        assert!(
            secret::ensure_no_undeclared_secret_references(&no_secrets(), &serialized).is_ok(),
            "literal text must not raise an undeclared-secret error"
        );
    }

    #[test]
    fn secrets_reference_inside_expression_is_detected_amid_literal_text() {
        // A genuine expression reference must still be caught even when unrelated
        // literal `secrets.*` text is present elsewhere in `with`.
        let serialized = serde_json::json!({
            "endpoint": {"uri": "https://example.com/secrets.PUBLIC_PATH"},
            "headers": {"Authorization": "${ \"Bearer \" + $secrets.API_TOKEN }"},
        })
        .to_string();
        let names = secret::referenced_secret_names(&serialized);
        assert!(names.contains("API_TOKEN"));
        // The literal path segment must not be picked up.
        assert!(!names.contains("PUBLIC_PATH"));
    }

    #[test]
    fn liquid_expression_secret_reference_is_detected() {
        let serialized = serde_json::json!({
            "headers": {"Authorization": "$${{{ 'Bearer ' | append: secrets.API_TOKEN }}}"},
        })
        .to_string();
        let names = secret::referenced_secret_names(&serialized);
        assert!(names.contains("API_TOKEN"));
    }

    #[test]
    fn liquid_secret_after_quoted_closing_delimiter_is_detected() {
        let serialized = serde_json::json!({
            "headers": {"Authorization": r#"$${{{ "}}" | append: secrets.API_TOKEN }}}"#},
        })
        .to_string();
        let names = secret::referenced_secret_names(&serialized);
        assert!(names.contains("API_TOKEN"));
    }

    #[test]
    fn liquid_secret_with_whitespace_control_is_detected() {
        let serialized = serde_json::json!({
            "headers": {"Authorization": "$${{{-secrets.API_TOKEN-}}}"},
        })
        .to_string();
        let names = secret::referenced_secret_names(&serialized);
        assert!(names.contains("API_TOKEN"), "{names:?}");
    }

    #[test]
    fn liquid_secret_after_unbalanced_quote_in_raw_block_is_detected() {
        for template in [
            r#"$${{% raw %}{{ " }}{% endraw %}{{ secrets.API_TOKEN }}}"#,
            r#"$${{% raw %}{{ "{% endraw %}{% if secrets.API_TOKEN %}x{% endif %}}"#,
            // The quote inside raw pairs with a quote after the real tag.
            r#"$${{% raw %}{{ "{% endraw %}{{ secrets.API_TOKEN }}" junk }}}"#,
        ] {
            let serialized =
                serde_json::json!({ "headers": {"Authorization": template} }).to_string();
            let names = secret::referenced_secret_names(&serialized);
            assert!(names.contains("API_TOKEN"), "{template}: {names:?}");
        }
    }

    #[test]
    fn liquid_secret_like_text_in_string_literal_or_raw_block_is_not_a_reference() {
        for template in [
            r#"$${{{ "{{ secrets.UNUSED }}" }}}"#,
            "$${{% raw %}{{ secrets.UNUSED }}{% endraw %}}",
            "$${{% comment %}{{ secrets.UNUSED }}{% endcomment %}}",
        ] {
            let serialized = serde_json::json!({ "headers": {"X-Doc": template} }).to_string();
            assert!(
                secret::referenced_secret_names(&serialized).is_empty(),
                "{template}"
            );
            assert!(
                secret::ensure_no_undeclared_secret_references(&no_secrets(), &serialized).is_ok(),
                "{template}"
            );
        }
    }

    #[test]
    fn liquid_output_literal_secret_like_text_is_not_a_reference() {
        let serialized = serde_json::json!({
            "headers": {"X-Path": "$${API path is secrets.PUBLIC_PATH}"},
        })
        .to_string();
        let names = secret::referenced_secret_names(&serialized);
        assert!(
            names.is_empty(),
            "Liquid output literals outside tags must not be treated as secret references"
        );
        assert!(secret::ensure_no_undeclared_secret_references(&no_secrets(), &serialized).is_ok());
    }

    #[test]
    fn secret_reference_after_earlier_expression_brace_is_detected() {
        let serialized = serde_json::json!({
            "headers": {
                "JqObjectLiteral": "${ {\"prefix\":\"Bearer \"}.prefix + $secrets.API_TOKEN }",
                "LiquidEarlierTag": "$${{{ user }}{{ secrets.API_TOKEN }}}",
            },
        })
        .to_string();
        let names = secret::referenced_secret_names(&serialized);
        assert!(names.contains("API_TOKEN"));
    }

    #[test]
    fn input_fields_named_like_secrets_are_not_secret_references() {
        let serialized = serde_json::json!({
            "headers": {
                "FromInput": "${ .secrets.API_TOKEN }",
                "NestedName": "${ .notsecrets.API_TOKEN }",
                "LiquidNestedName": "$${ notsecrets.API_TOKEN }",
                "LiquidBracketNestedName": "$${ notsecrets[\"API_TOKEN\"] }",
            },
        })
        .to_string();
        let names = secret::referenced_secret_names(&serialized);
        assert!(
            names.is_empty(),
            "only actual $secrets/secrets globals should be treated as secret references"
        );
        assert!(secret::ensure_no_undeclared_secret_references(&no_secrets(), &serialized).is_ok());
    }

    #[test]
    fn quoted_secret_like_text_inside_expression_is_not_a_reference() {
        let serialized = serde_json::json!({
            "headers": {
                "JqDoubleQuoted": "${ \"https://example.com/$secrets.API_TOKEN\" }",
                "JqBracketQuoted": "${ \"$secrets[\\\"github-token\\\"]\" }",
                "LiquidSingleQuoted": "$${ 'secrets.API_TOKEN' }",
                "LiquidDoubleQuoted": "$${ \"secrets[\\\"github-token\\\"]\" }",
            },
        })
        .to_string();
        let names = secret::referenced_secret_names(&serialized);
        assert!(
            names.is_empty(),
            "secret-like text inside expression string literals must not be treated as references"
        );
        assert!(secret::ensure_no_undeclared_secret_references(&no_secrets(), &serialized).is_ok());
    }

    #[test]
    fn non_ascii_literal_text_does_not_panic_secret_scan() {
        let serialized = serde_json::json!({
            "headers": {"X-Message": "こんにちは"},
            "body": "認証情報は使いません",
        })
        .to_string();
        assert!(secret::referenced_secret_names(&serialized).is_empty());
        assert!(secret::ensure_no_undeclared_secret_references(&no_secrets(), &serialized).is_ok());
    }

    #[test]
    fn resolve_declared_secrets_skips_secret_only_in_literal_text() {
        // A declared secret that appears only as literal text (an endpoint URI
        // path segment) is not an expression reference, so its env var must not be
        // required. With no env set, resolution returns an empty object instead of
        // erroring.
        let serialized = serde_json::json!({
            "endpoint": {"uri": "https://example.com/secrets.API_TOKEN"},
        })
        .to_string();
        let resolved =
            secret::resolve_declared_secrets(&declared(&["API_TOKEN"]), &serialized).unwrap();
        assert_eq!(resolved, serde_json::json!({}));
    }

    #[test]
    fn resolve_declared_secrets_requires_env_for_expression_reference() {
        // When the secret is referenced inside an expression span, its env var is
        // required; absent it, resolution errors (does not silently skip).
        let serialized = serde_json::json!({
            "headers": {"Authorization": "${ \"Bearer \" + $secrets.API_TOKEN }"},
        })
        .to_string();
        // SAFETY: tests run single-threaded (`--test-threads=1`).
        unsafe { std::env::remove_var("WORKFLOW_SECRET_API_TOKEN") };
        let err =
            secret::resolve_declared_secrets(&declared(&["API_TOKEN"]), &serialized).unwrap_err();
        assert!(err.to_string().contains("API_TOKEN"));
    }

    #[test]
    fn error_detail_within_limit_is_unchanged() {
        let body = "a".repeat(ERROR_DETAIL_MAX_BYTES);
        assert_eq!(CallTaskExecutor::truncate_error_detail(&body), body);
    }

    #[test]
    fn error_detail_over_limit_is_truncated_with_marker() {
        let body = "a".repeat(ERROR_DETAIL_MAX_BYTES + 1);
        let detail = CallTaskExecutor::truncate_error_detail(&body);
        assert_eq!(
            detail,
            format!("{}{}", "a".repeat(ERROR_DETAIL_MAX_BYTES), TRUNCATED_MARKER)
        );
    }

    #[test]
    fn error_detail_truncation_cuts_at_utf8_boundary() {
        // A 3-byte character straddling the limit must be dropped whole.
        let body = format!("{}あ", "a".repeat(ERROR_DETAIL_MAX_BYTES - 1));
        let detail = CallTaskExecutor::truncate_error_detail(&body);
        assert_eq!(
            detail,
            format!(
                "{}{}",
                "a".repeat(ERROR_DETAIL_MAX_BYTES - 1),
                TRUNCATED_MARKER
            )
        );
    }

    #[test]
    fn mask_error_masks_secrets_in_title_and_detail() {
        let error = workflow::errors::ErrorFactory::new().communication(
            "failed with s3cr3t".to_string(),
            None,
            Some("url: https://example.com/?key=s3cr3t%2Fvalue".to_string()),
        );
        let secrets = vec!["s3cr3t/value".to_string(), "s3cr3t".to_string()];

        let masked = CallTaskExecutor::mask_error(error, &secrets);

        let text = format!("{:?} {:?}", masked.title, masked.detail);
        assert!(!text.contains("s3cr3t"), "{text}");
        assert!(text.contains("example.com"), "{text}");
    }

    #[test]
    fn mask_error_without_secrets_keeps_error() {
        let error = workflow::errors::ErrorFactory::new().communication(
            "failed".to_string(),
            None,
            Some("a%2Fb".to_string()),
        );

        let masked = CallTaskExecutor::mask_error(error.clone(), &[]);

        assert_eq!(masked, error);
    }
}
