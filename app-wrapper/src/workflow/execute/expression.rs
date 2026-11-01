use super::context::{TaskContext, WorkflowContext};
use crate::workflow::definition::workflow;
use anyhow::Result;
pub use infra::workflow::definition::ExpressionContext;
use std::sync::Arc;

pub trait UseExpression {
    /// Variables for evaluating the expressions of one task step.
    ///
    /// `$input`, `$output` and context variables are shared pointers. The
    /// serialized views (`$context`, `$runtime`, `$workflow`, `$task`) are only
    /// built if an expression references them, from a snapshot taken here, so
    /// later changes to the workflow are not visible through this context.
    fn expression(
        workflow_context: &WorkflowContext,
        task_context: Arc<TaskContext>,
    ) -> impl std::future::Future<Output = Result<ExpressionContext, Box<workflow::Error>>> + Send
    {
        async move {
            let mut expression = ExpressionContext::new();
            expression.insert("input", task_context.input.clone());
            expression.insert("output", task_context.output.clone());

            let wf = Arc::new(workflow_context.clone());
            let w = wf.clone();
            expression.insert_lazy("context", move || serde_json::to_value(&*w));
            let w = wf.clone();
            expression.insert_lazy("runtime", move || {
                serde_json::to_value(w.to_runtime_descriptor())
            });
            expression.insert_lazy("workflow", move || serde_json::to_value(wf.to_descriptor()));
            let task = task_context.clone();
            expression.insert_lazy("task", move || serde_json::to_value(task.to_descriptor()));

            // Workflow variables are applied last so they win over task-scoped
            // ones and the built-in names, as before.
            for (k, v) in task_context.context_variables.lock().await.iter() {
                expression.insert(k.clone(), v.clone());
            }
            for (k, v) in workflow_context.context_variables.lock().await.iter() {
                expression.insert(k.clone(), v.clone());
            }
            Ok(expression)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Expr;
    impl UseExpression for Expr {}

    async fn build(wf: &WorkflowContext, task: &TaskContext) -> ExpressionContext {
        Expr::expression(wf, Arc::new(task.clone())).await.unwrap()
    }

    fn value(ctx: &ExpressionContext, name: &str) -> serde_json::Value {
        (*ctx.get(name).unwrap().expect(name)).clone()
    }

    #[tokio::test]
    async fn descriptors_match_direct_serialization() {
        let mut wf = WorkflowContext::new_empty();
        wf.input = Arc::new(json!({"wf_in": 1}));
        wf.output = Some(Arc::new(json!({"wf_out": 2})));
        let mut task = TaskContext::new_empty();
        task.set_input(Arc::new(json!({"task_in": 3})));
        let ctx = build(&wf, &task).await;

        assert_eq!(value(&ctx, "context"), serde_json::to_value(&wf).unwrap());
        assert_eq!(
            value(&ctx, "runtime"),
            serde_json::to_value(wf.to_runtime_descriptor()).unwrap()
        );
        assert_eq!(
            value(&ctx, "workflow"),
            serde_json::to_value(wf.to_descriptor()).unwrap()
        );
        assert_eq!(
            value(&ctx, "task"),
            serde_json::to_value(task.to_descriptor()).unwrap()
        );
        assert_eq!(value(&ctx, "input"), json!({"task_in": 3}));
        assert_eq!(value(&ctx, "output"), json!({"task_in": 3}));
    }

    #[tokio::test]
    async fn descriptors_are_snapshots_taken_at_build_time() {
        let mut wf = WorkflowContext::new_empty();
        wf.output = Some(Arc::new(json!("before")));
        let task = TaskContext::new_empty();
        let ctx = build(&wf, &task).await;
        wf.output = Some(Arc::new(json!("after")));
        assert_eq!(value(&ctx, "context")["output"], json!("before"));
    }

    #[tokio::test]
    async fn context_variables_are_shared_and_workflow_scope_wins() {
        let mut wf = WorkflowContext::new_empty();
        let task = TaskContext::new_empty();
        task.add_context_value("both".into(), json!("task")).await;
        task.add_context_value("task_only".into(), json!(1)).await;
        wf.add_context_value("both".into(), json!("workflow")).await;
        wf.add_context_value("input".into(), json!("shadowed"))
            .await;
        let ctx = build(&wf, &task).await;
        assert_eq!(value(&ctx, "both"), json!("workflow"));
        assert_eq!(value(&ctx, "task_only"), json!(1));
        assert_eq!(value(&ctx, "input"), json!("shadowed"));

        // Values are shared, not copied.
        let stored = wf
            .context_variables
            .lock()
            .await
            .get("both")
            .cloned()
            .unwrap();
        assert!(Arc::ptr_eq(&stored, &ctx.get("both").unwrap().unwrap()));
    }
}
