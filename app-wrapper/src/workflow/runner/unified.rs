//! Unified Workflow Runner implementation for app-wrapper
//!
//! This module provides a unified Workflow runner that supports 'run' and 'create' methods
//! via the `using` parameter.
//!
//! # Settings
//! Uses `WorkflowRunnerSettings` with optional `workflow_source`:
//! - If workflow_source is specified in settings, 'run' method can use it without args override
//! - If workflow_source is not in settings, 'run' method args must include workflow_source
//! - 'create' method always uses workflow_source from args (ignores settings)

use crate::modules::AppWrapperModule;
use crate::workflow::create_workflow::CreateWorkflowRunnerImpl;
use crate::workflow::definition::workflow::WorkflowSchema;
use crate::workflow::execute::checkpoint::CheckPointContext;
use crate::workflow::execute::task::ExecutionId;
use crate::workflow::execute::workflow::WorkflowExecutor;
use anyhow::{Result, anyhow};
use app::module::AppModule;
use async_trait::async_trait;
use command_utils::trace::Tracing;
use command_utils::trace::attr::langfuse_keys;
use futures::stream::BoxStream;
use futures::{StreamExt, pin_mut};
use jobworkerp_base::APP_NAME;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus;
use jobworkerp_runner::jobworkerp::runner::workflow_run_args::WorkflowSource as ArgsWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource as SettingsWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::{
    WorkflowResult, WorkflowRunArgs, WorkflowRunnerSettings,
};
use jobworkerp_runner::runner::cancellation::CancelMonitoring;
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::workflow_unified::{
    METHOD_CREATE, METHOD_RUN, WorkflowUnifiedRunnerSpecImpl,
};
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use opentelemetry::trace::TraceContextExt;
use prost::Message;
use proto::jobworkerp::data::{JobData, JobId, JobResult, ResultOutputItem, RunnerType};
use std::collections::HashMap;
use std::sync::Arc;

/// Unified Workflow Runner implementation that supports 'run' and 'create' methods
pub struct WorkflowUnifiedRunnerImpl {
    app_wrapper_module: Arc<AppWrapperModule>,
    app_module: Arc<AppModule>,
    /// Workflow from settings (optional, used by 'run' method if args don't specify workflow_source)
    settings_workflow: Option<Arc<WorkflowSchema>>,
    /// Workflow context from settings (optional, pre-parsed in load()). Merged with args.workflow_context (settings keys take precedence).
    settings_workflow_context: Option<Arc<serde_json::Value>>,
    /// Create runner for 'create' method
    create_runner: CreateWorkflowRunnerImpl,
    spec: WorkflowUnifiedRunnerSpecImpl,
    cancel_helper: Option<CancelMonitoringHelper>,
}

/// Structured, server-created details for a workflow runner error.
///
/// The contained `WorkflowResult` is available only by downcasting the
/// in-process runner error; it is not encoded as a successful runner result or
/// appended to the guest-visible diagnostic string.
pub struct WorkflowExecutionFailure {
    workflow_result: WorkflowResult,
    original_cause: Box<crate::workflow::definition::workflow::Error>,
}

impl WorkflowExecutionFailure {
    fn new(
        workflow_result: WorkflowResult,
        original_cause: Box<crate::workflow::definition::workflow::Error>,
    ) -> Self {
        Self {
            workflow_result,
            original_cause,
        }
    }

    /// The server-produced workflow status and receipt envelope for this
    /// failed execution. Callers must continue to treat the runner outcome as
    /// an error, not as a successful `WorkflowResult`.
    pub fn workflow_result(&self) -> &WorkflowResult {
        &self.workflow_result
    }

    /// Original workflow failure preserved independently of its diagnostic
    /// formatting, for internal error transport and classification.
    pub fn original_cause(&self) -> &crate::workflow::definition::workflow::Error {
        &self.original_cause
    }
}

impl std::fmt::Debug for WorkflowExecutionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowExecutionFailure")
            .field("original_cause", &self.original_cause)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for WorkflowExecutionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Failed to execute workflow: {:?}",
            self.original_cause
        )
    }
}

impl std::error::Error for WorkflowExecutionFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.original_cause.as_ref())
    }
}

/// Merge settings and args workflow contexts. Settings keys take precedence on conflict.
fn merge_workflow_contexts(
    settings_context: &Option<Arc<serde_json::Value>>,
    args_context: &Option<String>,
) -> Result<Arc<serde_json::Value>> {
    let args_map = match args_context.as_deref() {
        Some(s) => {
            let v: serde_json::Value = serde_json::from_str(s)
                .map_err(|e| anyhow!("Invalid workflow_context JSON in args: {}", e))?;
            match v {
                serde_json::Value::Object(m) => Some(m),
                _ => return Err(anyhow!("workflow_context in args must be a JSON object")),
            }
        }
        None => None,
    };

    match (settings_context, args_map) {
        (Some(settings_ctx), Some(mut args_map)) => {
            if let serde_json::Value::Object(settings_map) = settings_ctx.as_ref() {
                args_map.extend(settings_map.clone());
            }
            Ok(Arc::new(serde_json::Value::Object(args_map)))
        }
        (Some(ctx), None) => Ok(Arc::clone(ctx)),
        (None, Some(args_map)) => Ok(Arc::new(serde_json::Value::Object(args_map))),
        (None, None) => Ok(Arc::new(serde_json::Value::Object(Default::default()))),
    }
}

impl WorkflowUnifiedRunnerImpl {
    pub fn new(
        app_wrapper_module: Arc<AppWrapperModule>,
        app_module: Arc<AppModule>,
    ) -> Result<Self> {
        Ok(Self {
            app_wrapper_module,
            app_module: app_module.clone(),
            settings_workflow: None,
            settings_workflow_context: None,
            create_runner: CreateWorkflowRunnerImpl::new(app_module)?,
            spec: WorkflowUnifiedRunnerSpecImpl::new(),
            cancel_helper: None,
        })
    }

    pub fn new_with_cancel_monitoring(
        app_wrapper_module: Arc<AppWrapperModule>,
        app_module: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Result<Self> {
        Ok(Self {
            app_wrapper_module,
            app_module: app_module.clone(),
            settings_workflow: None,
            settings_workflow_context: None,
            create_runner: CreateWorkflowRunnerImpl::new_with_cancel_monitoring(
                app_module,
                cancel_helper.clone(),
            ),
            spec: WorkflowUnifiedRunnerSpecImpl::new(),
            cancel_helper: Some(cancel_helper),
        })
    }

    /// Convert SettingsWorkflowSource to ArgsWorkflowSource for unified handling
    fn convert_settings_source(source: &SettingsWorkflowSource) -> ArgsWorkflowSource {
        match source {
            SettingsWorkflowSource::WorkflowUrl(url) => {
                ArgsWorkflowSource::WorkflowUrl(url.clone())
            }
            SettingsWorkflowSource::WorkflowData(data) => {
                ArgsWorkflowSource::WorkflowData(data.clone())
            }
        }
    }

    /// The cancellation token this runner watches, if a cancel monitor is wired.
    /// Both `execute_run` and `execute_run_stream` use this to observe aborts.
    async fn optional_cancel_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        match &self.cancel_helper {
            Some(helper) => Some(helper.get_cancellation_token().await),
            None => None,
        }
    }

    /// Resolve once the token is cancelled, or never if there is no token. Lets
    /// the execution loops `select!` on cancellation uniformly whether or not a
    /// monitor is present.
    async fn await_cancel(token: &Option<tokio_util::sync::CancellationToken>) {
        match token {
            Some(t) => t.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    }

    /// How long the cancel-drain loop waits for in-flight tasks to unwind
    /// after `executor.cancel()` before giving up and synthesising a
    /// `Cancelled` result. The drain is best-effort: it lets tasks whose
    /// child jobs are already `Running` (and therefore got a broadcast
    /// cancel) finish observing the cancellation, but it must NOT depend
    /// on tasks that are blocked on `subscribe_result` for child jobs
    /// still in `Pending` — `cancel_job` only flips Pending → Cancelling
    /// and the result is not published until the dispatcher eventually
    /// pops the job. That can take up to the per-job direct-response
    /// timeout, which in workflow setups is routinely 30 minutes+, so
    /// blanket-draining the stream would defeat user-visible cancel.
    ///
    /// 2 seconds covers the common case of letting an already-running
    /// child task observe its broadcast cancel and yield a Cancelled
    /// task context, while staying within "feels immediate" for a
    /// user-initiated cancel.
    const CANCEL_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

    fn workflow_result_from_context(
        context: &crate::workflow::execute::context::WorkflowContext,
    ) -> WorkflowResult {
        WorkflowResult {
            id: context.id.to_string(),
            output: serde_json::to_string(&context.output).unwrap_or_default(),
            position: context.position.as_json_pointer(),
            status: WorkflowStatus::from_str_name(context.status.to_string().as_str())
                .unwrap_or(WorkflowStatus::Faulted) as i32,
            error_message: if context.status
                == crate::workflow::execute::context::WorkflowStatus::Completed
            {
                None
            } else {
                context.output.as_ref().map(|output| output.to_string())
            },
            child_execution_receipts: context.child_execution_receipts(),
        }
    }

    /// Take a snapshot of the workflow context for cases where the
    /// stream did not yield a final context before the cancel-drain
    /// deadline. We must synthesise a `Cancelled` `WorkflowResult` from
    /// whatever is on `workflow_context` so the caller's job result is
    /// not silently empty.
    async fn synthesize_cancelled_result_from_executor(
        executor: &WorkflowExecutor,
    ) -> Result<WorkflowResult> {
        let ctx = executor.workflow_context.read().await;
        Self::synthesize_cancelled_result_from_context(&ctx)
    }

    /// Pure-function variant kept separate from `_from_executor` so the
    /// `Arc<RwLock<WorkflowContext>>` lock acquisition can stay in one
    /// place. Used by the synth path above and exercised directly by
    /// unit tests that build a `WorkflowContext` without spinning up a
    /// full executor.
    fn synthesize_cancelled_result_from_context(
        ctx: &crate::workflow::execute::context::WorkflowContext,
    ) -> Result<WorkflowResult> {
        let output = ctx
            .output
            .as_ref()
            .map(|o| serde_json::to_string(o.as_ref()))
            .unwrap_or_else(|| Ok(String::new()))?;
        Ok(WorkflowResult {
            id: ctx.id.to_string(),
            output,
            position: ctx.position.as_json_pointer(),
            status: WorkflowStatus::Cancelled as i32,
            error_message: Some(
                "Workflow was cancelled; in-flight tasks did not finish within the drain window"
                    .to_string(),
            ),
            child_execution_receipts: ctx.child_execution_receipts(),
        })
    }

    /// Resolve workflow: args workflow_source takes precedence over settings
    /// Uses WorkflowLoader for proper URL/file loading
    async fn resolve_workflow(
        &self,
        args_source: Option<&ArgsWorkflowSource>,
    ) -> Result<Arc<WorkflowSchema>> {
        // Args workflow_source takes precedence
        if let Some(source) = args_source {
            let workflow = self
                .app_module
                .workflow_loader
                .load_workflow_source(source)
                .await?;
            return Ok(Arc::new(workflow));
        }

        // Fall back to settings workflow
        self.settings_workflow
            .clone()
            .ok_or_else(|| anyhow!("No workflow_source specified in settings or args"))
    }

    /// Resolve workflow context: merges settings and args contexts (settings keys take precedence)
    fn resolve_workflow_context(
        &self,
        args_context: &Option<String>,
    ) -> Result<Arc<serde_json::Value>> {
        merge_workflow_contexts(&self.settings_workflow_context, args_context)
    }

    fn workflow_execution_error(
        context: &crate::workflow::execute::context::WorkflowContext,
        cause: Box<crate::workflow::definition::workflow::Error>,
    ) -> anyhow::Error {
        let failure =
            WorkflowExecutionFailure::new(Self::workflow_result_from_context(context), cause);
        let runner_error = JobWorkerError::RuntimeError(failure.to_string());
        // Keep JobWorkerError as an anyhow cause so existing worker retry and
        // failure classification remains unchanged, while exposing the typed
        // workflow payload as an error context for internal downcasting.
        anyhow::Error::new(runner_error).context(failure)
    }

    /// Collects the terminal workflow result without converting a yielded
    /// workflow error into a successful protobuf result. Kept separate from
    /// argument resolution to make the worker-error handoff path testable with
    /// an executor that already has server-observed child receipts.
    async fn execute_workflow_result(
        executor: Arc<WorkflowExecutor>,
        cx: Arc<opentelemetry::Context>,
        cancel_token: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<WorkflowResult> {
        let workflow_stream = executor.execute_workflow(cx);
        pin_mut!(workflow_stream);

        let mut final_context = None;
        let mut cancelled = false;
        loop {
            let next = tokio::select! {
                biased;
                _ = Self::await_cancel(&cancel_token) => {
                    executor.cancel().await;
                    cancelled = true;
                    // Bounded drain: give in-flight tasks a short window to
                    // unwind so already-running children that observe the
                    // broadcast cancel can publish a Cancelled task result.
                    let drain_deadline = tokio::time::Instant::now() + Self::CANCEL_DRAIN_TIMEOUT;
                    while let Ok(Some(result)) = tokio::time::timeout_at(
                        drain_deadline,
                        workflow_stream.next(),
                    ).await {
                        match result {
                            Ok(context) => final_context = Some(context),
                            Err(error) => {
                                let failure_context = match final_context.take() {
                                    Some(context) => context,
                                    None => Arc::new(executor.workflow_context.read().await.clone()),
                                };
                                return Err(Self::workflow_execution_error(&failure_context, error));
                            }
                        }
                    }
                    break;
                }
                item = workflow_stream.next() => item,
            };
            match next {
                Some(Ok(context)) => {
                    final_context = Some(context);
                }
                Some(Err(error)) => {
                    let failure_context = match final_context.take() {
                        Some(context) => context,
                        None => Arc::new(executor.workflow_context.read().await.clone()),
                    };
                    return Err(Self::workflow_execution_error(&failure_context, error));
                }
                None => break,
            }
        }

        if let Some(context) = final_context {
            tracing::info!("Workflow result: {}", context.output_string());
            Ok(Self::workflow_result_from_context(&context))
        } else if cancelled {
            // Drain window elapsed without any context being yielded.
            tracing::warn!(
                "Workflow cancel drain window elapsed without a final context; \
                 synthesising Cancelled result from executor state"
            );
            Self::synthesize_cancelled_result_from_executor(&executor).await
        } else {
            Err(anyhow!("No workflow context was returned"))
        }
    }

    /// Execute workflow run (implementation for 'run' method)
    async fn execute_run(
        &self,
        args: &WorkflowRunArgs,
        metadata: HashMap<String, String>,
    ) -> Result<Vec<u8>> {
        use opentelemetry::trace::Span;
        let mut span = Self::otel_span_from_metadata(&metadata, APP_NAME, "workflow.run");
        // Record input now, output just before drop, so Langfuse shows both columns.
        span.set_attribute(opentelemetry::KeyValue::new(
            langfuse_keys::OBSERVATION_INPUT,
            args.input.clone(),
        ));
        let cx = opentelemetry::Context::current_with_span(span);
        let execution_id = ExecutionId::new_opt(args.execution_id.clone());

        // Check for cancellation
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if token.is_cancelled() {
                return Err(anyhow!(
                    "canceled by user: {}, {:?}",
                    RunnerType::Workflow.as_str_name(),
                    args
                ));
            }
        }

        // Resolve workflow (args takes precedence over settings)
        let workflow = self.resolve_workflow(args.workflow_source.as_ref()).await?;
        tracing::debug!("Workflow resolved: {:#?}", &workflow);

        let input_json = serde_json::from_str(&args.input)
            .unwrap_or_else(|_| serde_json::Value::String(args.input.clone()));
        let context_json = self.resolve_workflow_context(&args.workflow_context)?;
        let chpoint = if let Some(ch) = args.from_checkpoint.as_ref() {
            Some(CheckPointContext::from_workflow_run(ch)?)
        } else {
            None
        };

        let executor = Arc::new(
            WorkflowExecutor::init(
                self.app_wrapper_module.clone(),
                self.app_module.clone(),
                workflow,
                Arc::new(input_json),
                execution_id,
                context_json,
                Arc::new(metadata),
                chpoint,
            )
            .await?,
        );

        // Watch the cancellation token alongside the workflow stream so an abort
        // takes effect at the next task boundary instead of waiting for the
        // whole workflow to finish. On cancel, `executor.cancel()` flips the
        // workflow status to Cancelled (which stops the task loop) and fans out
        // delete_job to every in-flight child job.
        let cancel_token = self.optional_cancel_token().await;

        let r = Self::execute_workflow_result(executor, Arc::new(cx.clone()), cancel_token).await?;
        // Stamp final output onto the root span before its Context is dropped.
        use opentelemetry::trace::TraceContextExt;
        cx.span().set_attribute(opentelemetry::KeyValue::new(
            langfuse_keys::OBSERVATION_OUTPUT,
            r.output.clone(),
        ));
        drop(cx);
        Ok(r.encode_to_vec())
    }

    /// Execute workflow run as stream (implementation for 'run' method streaming)
    async fn execute_run_stream(
        &self,
        args: &WorkflowRunArgs,
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        use opentelemetry::trace::Span;
        // Without an explicit root span here, `create_context` returns an empty Context
        // and downstream child spans end up with a zero trace_id — the trace then never
        // surfaces in Langfuse.
        let mut span = Self::otel_span_from_metadata(&metadata, APP_NAME, "workflow.run_stream");
        span.set_attribute(opentelemetry::KeyValue::new(
            langfuse_keys::OBSERVATION_INPUT,
            args.input.clone(),
        ));
        let cx = opentelemetry::Context::current_with_span(span);
        let metadata_arc = Arc::new(metadata.clone());
        let execution_id = ExecutionId::new_opt(args.execution_id.clone());

        // Check for cancellation
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if token.is_cancelled() {
                return Err(anyhow!(
                    "canceled by user: {}, {:?}",
                    RunnerType::Workflow.as_str_name(),
                    args
                ));
            }
        }

        // Resolve workflow (args takes precedence over settings)
        let workflow = self.resolve_workflow(args.workflow_source.as_ref()).await?;

        let input_json = serde_json::from_str(&args.input)
            .unwrap_or_else(|_| serde_json::Value::String(args.input.clone()));
        let context_json = self.resolve_workflow_context(&args.workflow_context)?;
        let chpoint = if let Some(ch) = args.from_checkpoint.as_ref() {
            Some(CheckPointContext::from_workflow_run(ch)?)
        } else {
            None
        };

        let executor = Arc::new(
            WorkflowExecutor::init(
                self.app_wrapper_module.clone(),
                self.app_module.clone(),
                workflow,
                Arc::new(input_json),
                execution_id,
                context_json,
                metadata_arc.clone(),
                chpoint,
            )
            .await?,
        );

        // Wrap the source stream so cancellation is observed between items:
        // on cancel, fan out delete_job to in-flight child jobs and stop the
        // workflow (status -> Cancelled), then drain (bounded) and
        // terminate. Without this the returned BoxStream would only end when
        // the workflow finished naturally, defeating prompt cancellation;
        // without the bound the drain itself would block on tasks awaiting
        // Pending child results (see CANCEL_DRAIN_TIMEOUT for why).
        //
        // `cancel_drained_without_final` is set when the drain window
        // elapsed without the inner stream yielding any further `Ok`
        // context. The closing `.chain` below uses it to emit a synthetic
        // `Cancelled` `WorkflowResult` so subscribers never see an empty
        // stream after a cancel.
        let cancel_token = self.optional_cancel_token().await;
        let executor_for_stream = executor.clone();
        let inner_cx = Arc::new(cx.clone());
        let cancel_drained_without_final = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_drained_flag = cancel_drained_without_final.clone();
        let workflow_stream = async_stream::stream! {
            let inner = executor_for_stream.execute_workflow(inner_cx);
            futures::pin_mut!(inner);
            loop {
                let next = tokio::select! {
                    biased;
                    _ = Self::await_cancel(&cancel_token) => {
                        executor_for_stream.cancel().await;
                        let drain_deadline = tokio::time::Instant::now()
                            + Self::CANCEL_DRAIN_TIMEOUT;
                        let mut saw_ok_item = false;
                        while let Ok(Some(item)) = tokio::time::timeout_at(
                            drain_deadline,
                            inner.next(),
                        ).await {
                            if item.is_ok() {
                                saw_ok_item = true;
                            }
                            yield item;
                        }
                        if !saw_ok_item {
                            cancel_drained_flag
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        break;
                    }
                    item = inner.next() => item,
                };
                match next {
                    Some(item) => yield item,
                    None => break,
                }
            }
        };
        // root_cx is moved into the trailing `once` future so the BoxedSpan stays
        // alive until the stream emits End — at that point the Context drops and the
        // span ends. last_output captures the most recent successful output so we can
        // stamp it onto the root span there. Faulted chunks are intentionally NOT
        // recorded so an error mid-stream doesn't blank out an earlier good value.
        let root_cx = cx;
        let last_output: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let last_output_for_then = last_output.clone();
        let executor_for_tail = executor.clone();
        let executor_for_items = executor.clone();
        let output_stream = workflow_stream
            .then(move |result| {
                let last_output = last_output_for_then.clone();
                let executor_for_items = executor_for_items.clone();
                async move {
                    let workflow_result = match result {
                        Ok(context) => Self::workflow_result_from_context(&context),
                        Err(e) => {
                            tracing::error!("Error in workflow execution: {:?}", e);
                            WorkflowResult {
                                id: "error".to_string(),
                                output: "".to_string(),
                                position: e.as_ref().instance.clone().unwrap_or_default(),
                                status: WorkflowStatus::Faulted as i32,
                                error_message: Some(format!("Failed to execute workflow: {e}")),
                                child_execution_receipts: executor_for_items
                                    .workflow_context
                                    .read()
                                    .await
                                    .child_execution_receipts(),
                            }
                        }
                    };
                    if workflow_result.status != WorkflowStatus::Faulted as i32
                        && let Ok(mut slot) = last_output.lock()
                    {
                        *slot = Some(workflow_result.output.clone());
                    }
                    ResultOutputItem {
                        item: Some(proto::jobworkerp::data::result_output_item::Item::Data(
                            workflow_result.encode_to_vec(),
                        )),
                    }
                }
            })
            // Drain elapsed without any `Ok` context: emit a synthetic
            // Cancelled result so subscribers see the cancel outcome
            // rather than an empty data stream followed by End. Skipped
            // when the workflow produced at least one context during
            // drain (the normal path).
            .chain(
                futures::stream::once(async move {
                    if cancel_drained_without_final.load(std::sync::atomic::Ordering::SeqCst) {
                        match Self::synthesize_cancelled_result_from_executor(&executor_for_tail)
                            .await
                        {
                            Ok(synth) => Some(ResultOutputItem {
                                item: Some(
                                    proto::jobworkerp::data::result_output_item::Item::Data(
                                        synth.encode_to_vec(),
                                    ),
                                ),
                            }),
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to synthesise Cancelled result on drain timeout: {:?}",
                                    e
                                );
                                None
                            }
                        }
                    } else {
                        None
                    }
                })
                .filter_map(|opt| async move { opt }),
            )
            .chain(futures::stream::once(async move {
                if let Some(output) = last_output.lock().ok().and_then(|s| s.clone()) {
                    use opentelemetry::trace::TraceContextExt;
                    root_cx.span().set_attribute(opentelemetry::KeyValue::new(
                        langfuse_keys::OBSERVATION_OUTPUT,
                        output,
                    ));
                }
                drop(root_cx);
                ResultOutputItem {
                    item: Some(proto::jobworkerp::data::result_output_item::Item::End(
                        proto::jobworkerp::data::Trailer { metadata },
                    )),
                }
            }))
            .boxed();

        Ok(output_stream)
    }
}

impl Tracing for WorkflowUnifiedRunnerImpl {}

impl UseCancelMonitoringHelper for WorkflowUnifiedRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

impl RunnerSpec for WorkflowUnifiedRunnerImpl {
    fn name(&self) -> String {
        self.spec.name()
    }

    fn runner_settings_proto(&self) -> String {
        self.spec.runner_settings_proto()
    }

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        self.spec.method_proto_map()
    }

    fn method_json_schema_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodJsonSchema> {
        self.spec.method_json_schema_map()
    }

    fn settings_schema(&self) -> String {
        self.spec.settings_schema()
    }

    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        using: Option<&str>,
    ) -> jobworkerp_runner::runner::CollectStreamFuture {
        self.spec.collect_stream(stream, using)
    }
}

#[async_trait]
impl RunnerTrait for WorkflowUnifiedRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        // Parse WorkflowRunnerSettings - workflow_source is optional
        if !settings.is_empty() {
            let parsed_settings =
                ProstMessageCodec::deserialize_message::<WorkflowRunnerSettings>(&settings)?;
            // If workflow_source is specified in settings, parse and store it
            if let Some(source) = parsed_settings.workflow_source {
                let args_source = Self::convert_settings_source(&source);
                let workflow = self
                    .app_module
                    .workflow_loader
                    .load_workflow_source(&args_source)
                    .await?;
                tracing::debug!("Workflow loaded from settings: {:#?}", &workflow);
                self.settings_workflow = Some(Arc::new(workflow));
            }
            // Parse, validate and store workflow_context from settings
            if let Some(ref ctx) = parsed_settings.workflow_context {
                let v: serde_json::Value = serde_json::from_str(ctx)
                    .map_err(|e| anyhow!("Invalid workflow_context JSON in settings: {}", e))?;
                if !v.is_object() {
                    return Err(anyhow!(
                        "workflow_context in settings must be a JSON object"
                    ));
                }
                self.settings_workflow_context = Some(Arc::new(v));
            }
        }
        // create_runner.load() is a no-op but call it for consistency
        self.create_runner.load(vec![]).await?;
        Ok(())
    }

    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        match WorkflowUnifiedRunnerSpecImpl::resolve_method(using) {
            Ok(METHOD_RUN) => {
                let args = match ProstMessageCodec::deserialize_message::<WorkflowRunArgs>(arg) {
                    Ok(args) => args,
                    Err(e) => return (Err(e), metadata),
                };
                let result = self.execute_run(&args, metadata.clone()).await;
                (result, metadata)
            }
            Ok(METHOD_CREATE) => self.create_runner.run(arg, metadata, None).await,
            Ok(_) => (
                Err(anyhow!("Internal error: unknown method after validation")),
                metadata,
            ),
            Err(e) => (Err(e), metadata),
        }
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        match WorkflowUnifiedRunnerSpecImpl::resolve_method(using) {
            Ok(METHOD_RUN) => {
                let args = ProstMessageCodec::deserialize_message::<WorkflowRunArgs>(arg)?;
                self.execute_run_stream(&args, metadata).await
            }
            Ok(METHOD_CREATE) => self.create_runner.run_stream(arg, metadata, None).await,
            Ok(_) => Err(anyhow!("Internal error: unknown method after validation")),
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl CancelMonitoring for WorkflowUnifiedRunnerImpl {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            Ok(None)
        }
    }

    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    async fn request_cancellation(&mut self) -> Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
            }
        }
        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobworkerp_runner::runner::RunnerSpec;
    use serde_json::json;

    #[test]
    fn test_merge_both_contexts_with_overlapping_keys() {
        let settings = Some(Arc::new(json!({"source": "settings", "priority": "high"})));
        let args = Some(r#"{"source": "args", "extra": "from_args"}"#.to_string());

        let result = merge_workflow_contexts(&settings, &args).unwrap();
        let obj = result.as_object().unwrap();

        assert_eq!(obj.get("source").unwrap(), "settings");
        assert_eq!(obj.get("priority").unwrap(), "high");
        assert_eq!(obj.get("extra").unwrap(), "from_args");
        assert_eq!(obj.len(), 3);
    }

    #[test]
    fn test_merge_both_contexts_no_overlap() {
        let settings = Some(Arc::new(json!({"a": 1})));
        let args = Some(r#"{"b": 2}"#.to_string());

        let result = merge_workflow_contexts(&settings, &args).unwrap();
        let obj = result.as_object().unwrap();

        assert_eq!(obj.get("a").unwrap(), 1);
        assert_eq!(obj.get("b").unwrap(), 2);
        assert_eq!(obj.len(), 2);
    }

    #[test]
    fn test_merge_settings_only() {
        let settings = Some(Arc::new(json!({"key": "value"})));
        let result = merge_workflow_contexts(&settings, &None).unwrap();
        assert_eq!(result.as_ref(), &json!({"key": "value"}));
    }

    #[test]
    fn test_merge_args_only() {
        let args = Some(r#"{"key": "value"}"#.to_string());
        let result = merge_workflow_contexts(&None, &args).unwrap();
        assert_eq!(result.as_ref(), &json!({"key": "value"}));
    }

    #[test]
    fn test_merge_neither() {
        let result = merge_workflow_contexts(&None, &None).unwrap();
        assert_eq!(result.as_ref(), &json!({}));
    }

    #[test]
    fn test_merge_invalid_args_json() {
        let settings = Some(Arc::new(json!({"a": 1})));
        let args = Some("not json".to_string());
        assert!(merge_workflow_contexts(&settings, &args).is_err());
    }

    #[test]
    fn test_merge_non_object_args() {
        let args = Some("[1,2,3]".to_string());
        assert!(merge_workflow_contexts(&None, &args).is_err());
    }

    #[test]
    fn test_resolve_method() {
        assert!(WorkflowUnifiedRunnerSpecImpl::resolve_method(Some("run")).is_ok());
        assert!(WorkflowUnifiedRunnerSpecImpl::resolve_method(Some("create")).is_ok());
        // Default to "run" when None
        assert!(WorkflowUnifiedRunnerSpecImpl::resolve_method(None).is_ok());
        assert_eq!(
            WorkflowUnifiedRunnerSpecImpl::resolve_method(None).unwrap(),
            "run"
        );
        assert!(WorkflowUnifiedRunnerSpecImpl::resolve_method(Some("unknown")).is_err());
    }

    #[test]
    fn test_runner_spec_name() {
        let spec = WorkflowUnifiedRunnerSpecImpl::new();
        assert_eq!(spec.name(), "WORKFLOW");
    }

    #[test]
    fn test_method_proto_map_has_both_methods() {
        let spec = WorkflowUnifiedRunnerSpecImpl::new();
        let methods = spec.method_proto_map();

        assert!(methods.contains_key("run"));
        assert!(methods.contains_key("create"));
        assert_eq!(methods.len(), 2);

        // Verify schemas are not empty
        let run = methods.get("run").unwrap();
        assert!(!run.args_proto.is_empty());
        assert!(!run.result_proto.is_empty());

        let create = methods.get("create").unwrap();
        assert!(!create.args_proto.is_empty());
        assert!(!create.result_proto.is_empty());
    }

    #[test]
    fn test_method_json_schema_map_has_both_methods() {
        let spec = WorkflowUnifiedRunnerSpecImpl::new();
        let schemas = spec.method_json_schema_map();

        assert!(schemas.contains_key("run"));
        assert!(schemas.contains_key("create"));
        assert_eq!(schemas.len(), 2);

        // Verify schemas are valid JSON
        for (method_name, schema) in &schemas {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&schema.args_schema);
            assert!(
                parsed.is_ok(),
                "Invalid JSON in args_schema for method '{}'",
                method_name
            );
        }
    }

    /// Drain-window fallback must produce a `Cancelled` `WorkflowResult`
    /// that carries the executor's current position and any partial
    /// output, so the caller never sees an empty / Faulted result after
    /// cancellation. Regression for the issue where a Pending child job
    /// kept `result_fut` parked until the per-job direct-response
    /// timeout (~tens of minutes for workflows).
    #[test]
    fn synthesize_cancelled_result_captures_partial_progress() {
        use crate::workflow::execute::context::WorkflowContext;
        let mut ctx = WorkflowContext::new_empty();
        ctx.output = Some(Arc::new(json!({"partial": true, "step": 3})));

        let r = WorkflowUnifiedRunnerImpl::synthesize_cancelled_result_from_context(&ctx)
            .expect("synth must succeed");

        assert_eq!(r.status, WorkflowStatus::Cancelled as i32);
        assert!(r.error_message.is_some());
        assert!(r.error_message.as_ref().unwrap().contains("cancel"));
        // Output is preserved (serialised JSON) so callers can inspect
        // how far the workflow progressed before the cancel.
        let parsed: serde_json::Value = serde_json::from_str(&r.output).unwrap();
        assert_eq!(parsed["partial"], json!(true));
        assert_eq!(parsed["step"], json!(3));
    }

    /// When the workflow had not produced any output yet, the synth path
    /// must still return a well-formed `Cancelled` result — output is
    /// empty rather than failing.
    #[test]
    fn synthesize_cancelled_result_handles_empty_output() {
        use crate::workflow::execute::context::WorkflowContext;
        let ctx = WorkflowContext::new_empty();

        let r = WorkflowUnifiedRunnerImpl::synthesize_cancelled_result_from_context(&ctx)
            .expect("synth must succeed");

        assert_eq!(r.status, WorkflowStatus::Cancelled as i32);
        assert_eq!(r.output, "");
        assert!(r.error_message.is_some());
    }

    #[test]
    fn workflow_results_propagate_private_receipts_for_all_terminal_statuses() {
        use crate::workflow::execute::context::WorkflowContext;
        use jobworkerp_runner::jobworkerp::runner::{
            ChildDurableLookupState, ChildExecutionReceipt, ChildProducerEof,
        };

        for status in [
            crate::workflow::execute::context::WorkflowStatus::Completed,
            crate::workflow::execute::context::WorkflowStatus::Faulted,
            crate::workflow::execute::context::WorkflowStatus::Cancelled,
        ] {
            let mut context = WorkflowContext::new_empty();
            context.status = status;
            context.record_child_execution_receipt(ChildExecutionReceipt {
                workflow_execution_id: context.id.to_string(),
                task_position: "/sandbox".to_string(),
                child_job_id: 501,
                worker_id: Some(11),
                worker_name: "sandbox-worker".to_string(),
                runner_id: Some(22),
                runner_name: "SANDBOX".to_string(),
                method_using: Some("run".to_string()),
                settings_sha256: vec![1; 32],
                method_schema_sha256: vec![2; 32],
                arguments_sha256: vec![3; 32],
                timeout_sec: Some(30),
                cli_exit_code: Some(7),
                end_received: true,
                protocol_failure: None,
                producer_eof: ChildProducerEof::Unknown as i32,
                child_job_result_id: None,
                child_job_result_status: None,
                durable_lookup_state: ChildDurableLookupState::Unknown as i32,
                store_success: Some(true),
                store_failure: Some(true),
                broadcast_results: Some(true),
                sandbox_observation: None,
            });

            let result = WorkflowUnifiedRunnerImpl::workflow_result_from_context(&context);
            let receipts = result.child_execution_receipts.as_ref().unwrap();
            assert_eq!(receipts.receipts.len(), 1);
            assert_eq!(receipts.receipts[0].cli_exit_code, Some(7));
            assert_eq!(
                result.status,
                WorkflowStatus::from_str_name(context.status.to_string().as_str()).unwrap() as i32
            );

            let decoded = WorkflowResult::decode(result.encode_to_vec().as_slice()).unwrap();
            assert_eq!(decoded.id, context.id.to_string());
            assert_eq!(decoded.status, result.status);
            assert_eq!(decoded.child_execution_receipts.unwrap().receipts.len(), 1);
        }
    }

    #[test]
    fn faulted_workflow_receipts_remain_on_runner_error_not_success_output() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::workflow::execute::context::WorkflowContext;
            use jobworkerp_runner::jobworkerp::runner::{
                ChildDurableLookupState, ChildExecutionReceipt, ChildProducerEof,
            };

            let workflow = Arc::new(
                serde_json::from_value::<WorkflowSchema>(json!({
                    "document": {
                        "dsl": "1.0.0",
                        "namespace": "test",
                        "name": "faulted-receipt-test",
                        "version": "1.0.0"
                    },
                    "input": {
                        "schema": {
                            "document": {
                                "type": "object",
                                "required": ["required_value"],
                                "properties": {"required_value": {"type": "string"}}
                            }
                        }
                    },
                    "do": [{"task": {"set": {"should_not_run": true}}}]
                }))
                .expect("workflow schema is valid"),
            );
            let app_module = Arc::new(app::module::test::create_hybrid_test_app().await.unwrap());
            let workflow_context =
                WorkflowContext::new(&workflow, Arc::new(json!({})), Arc::new(json!({})), None);
            workflow_context.record_child_execution_receipt(ChildExecutionReceipt {
                workflow_execution_id: workflow_context.id.to_string(),
                task_position: "/ROOT/do/previous_sandbox".to_string(),
                child_job_id: 777,
                worker_id: Some(11),
                worker_name: "sandbox-worker".to_string(),
                runner_id: Some(22),
                runner_name: "SANDBOX".to_string(),
                method_using: Some("run".to_string()),
                settings_sha256: vec![1; 32],
                method_schema_sha256: vec![2; 32],
                arguments_sha256: vec![3; 32],
                timeout_sec: Some(30),
                cli_exit_code: Some(7),
                end_received: true,
                protocol_failure: None,
                producer_eof: ChildProducerEof::Unknown as i32,
                child_job_result_id: None,
                child_job_result_status: None,
                durable_lookup_state: ChildDurableLookupState::Unknown as i32,
                store_success: Some(true),
                store_failure: Some(true),
                broadcast_results: Some(true),
                sandbox_observation: None,
            });
            let executor = Arc::new(WorkflowExecutor {
                default_task_timeout_sec: 30,
                job_executors: Arc::new(app::app::job::execute::JobExecutorWrapper::new(
                    app_module,
                )),
                workflow,
                workflow_context: Arc::new(tokio::sync::RwLock::new(workflow_context)),
                execution_id: None,
                metadata: Arc::new(HashMap::new()),
                checkpoint_repository: None,
            });

            let error = WorkflowUnifiedRunnerImpl::execute_workflow_result(
                executor,
                Arc::new(opentelemetry::Context::current()),
                None,
            )
            .await
            .expect_err("input validation must remain a runner error");

            let failure = error
                .downcast_ref::<WorkflowExecutionFailure>()
                .expect("workflow error should carry a typed internal payload");
            let result = failure.workflow_result();
            assert_eq!(result.status, WorkflowStatus::Faulted as i32);
            let receipts = result.child_execution_receipts.as_ref().unwrap();
            assert_eq!(receipts.receipts.len(), 1);
            assert_eq!(receipts.receipts[0].child_job_id, 777);
            assert_eq!(
                receipts.receipts[0].durable_lookup_state,
                ChildDurableLookupState::Unknown as i32
            );
            assert!(
                failure
                    .original_cause()
                    .to_string()
                    .contains("Workflow input validation failed")
            );
            assert!(std::error::Error::source(failure).is_some());
            assert!(matches!(
                error.downcast_ref::<JobWorkerError>(),
                Some(JobWorkerError::RuntimeError(_))
            ));
            assert!(!error.to_string().contains("child_execution_receipts"));
            assert!(!error.to_string().contains("777"));
        });
    }

    #[test]
    fn new_workflow_result_decodes_legacy_fields_without_changing_them() {
        #[derive(Clone, PartialEq, prost::Message)]
        struct LegacyWorkflowResult {
            #[prost(string, tag = "1")]
            id: String,
            #[prost(string, tag = "2")]
            output: String,
            #[prost(string, tag = "3")]
            position: String,
            #[prost(int32, tag = "4")]
            status: i32,
            #[prost(string, optional, tag = "5")]
            error_message: Option<String>,
        }

        let legacy = WorkflowResult {
            id: "workflow-old-client".to_string(),
            output: "{\"ok\":true}".to_string(),
            position: "/task".to_string(),
            status: WorkflowStatus::Faulted as i32,
            error_message: Some("legacy error".to_string()),
            child_execution_receipts: None,
        };
        let decoded = WorkflowResult::decode(legacy.encode_to_vec().as_slice()).unwrap();

        assert_eq!(decoded.id, "workflow-old-client");
        assert_eq!(decoded.output, "{\"ok\":true}");
        assert_eq!(decoded.position, "/task");
        assert_eq!(decoded.status, WorkflowStatus::Faulted as i32);
        assert_eq!(decoded.error_message.as_deref(), Some("legacy error"));
        assert!(decoded.child_execution_receipts.is_none());

        let with_receipts = WorkflowResult {
            child_execution_receipts: Some(
                jobworkerp_runner::jobworkerp::runner::ChildExecutionReceipts {
                    schema_version: 1,
                    receipts: Vec::new(),
                    collection_incomplete: true,
                },
            ),
            ..legacy
        };
        let decoded_by_legacy =
            LegacyWorkflowResult::decode(with_receipts.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded_by_legacy.id, "workflow-old-client");
        assert_eq!(decoded_by_legacy.output, "{\"ok\":true}");
        assert_eq!(decoded_by_legacy.position, "/task");
        assert_eq!(decoded_by_legacy.status, WorkflowStatus::Faulted as i32);
        assert_eq!(
            decoded_by_legacy.error_message.as_deref(),
            Some("legacy error")
        );
    }
}
