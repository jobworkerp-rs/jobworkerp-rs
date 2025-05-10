use tokio::sync::RwLock;

use crate::workflow::{
    definition::workflow,
    execute::{
        context::{TaskContext, Then, WorkflowContext},
        task::{Result, TaskExecutorTrait},
    },
};
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::SwitchTask,
    },
    execute::expression::UseExpression,
};
use std::sync::Arc;
pub struct SwitchTaskExecutor {
    switch_task: SwitchTask,
}

impl SwitchTaskExecutor {
    pub fn new(switch_task: &SwitchTask) -> Self {
        Self {
            switch_task: switch_task.clone(),
        }
    }
}

impl UseExpression for SwitchTaskExecutor {}
impl UseExpressionTransformer for SwitchTaskExecutor {}
impl UseJqAndTemplateTransformer for SwitchTaskExecutor {}

impl TaskExecutorTrait<'_> for SwitchTaskExecutor {
    async fn execute(
        &self,
        _task_id: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        tracing::debug!("SwitchTaskExecutor: {}", _task_id);
        task_context.output = task_context.input.clone();
        task_context.add_position_name("switch".to_string()).await;

        // find match case
        let mut matched = false;
        let expression = match Self::expression(
            &*workflow_context.read().await,
            Arc::new(task_context.clone()),
        )
        .await
        {
            Ok(e) => e,
            Err(mut e) => {
                let pos = task_context.position.lock().await.clone();
                e.position(&pos);
                return Err(e);
            }
        };

        for switch_item in &self.switch_task.switch {
            // Each case contains only one element in the format of HashMap<String, SwitchCase>
            for (case_name, switch_case) in switch_item.iter() {
                // when condition
                let matched_case = if let Some(when) = &switch_case.when {
                    match Self::execute_transform_as_bool(
                        task_context.input.clone(),
                        when,
                        &expression,
                    ) {
                        Ok(matched) => matched,
                        Err(mut e) => {
                            let mut pos = task_context.position.lock().await.clone();
                            pos.push("when".to_string());
                            e.position(&pos);
                            return Err(e);
                        }
                    }
                } else {
                    // default
                    true
                };
                if matched_case {
                    tracing::debug!("Switch case matched: {}", case_name);
                    matched = true;

                    task_context.flow_directive = match Then::create(
                        task_context.output.clone(),
                        &switch_case.then,
                        &expression,
                    ) {
                        Ok(v) => v,
                        Err(mut e) => {
                            tracing::error!("Failed to evaluate switch `then' condition: {:#?}", e);
                            task_context.add_position_name("then".to_string()).await;
                            e.position(&task_context.position.lock().await.clone());
                            return Err(e);
                        }
                    };
                    break;
                }
            }

            if matched {
                break;
            }
        }
        task_context.remove_position().await;
        Ok(task_context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::workflow::{FlowDirective, SwitchCase, SwitchTask};
    use crate::workflow::execute::context::{TaskContext, WorkflowContext};
    use crate::workflow::execute::task::TaskExecutorTrait;
    use serde_json::{json, Map};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn create_switch_task(cases: Vec<(String, Option<String>, String)>) -> SwitchTask {
        let mut switch_items = Vec::new();

        for (case_name, when_expr, then_value) in cases {
            let mut case_map = HashMap::new();
            let flow_directive = FlowDirective::Variant1(then_value);

            let switch_case = SwitchCase {
                when: when_expr,
                then: flow_directive,
            };

            case_map.insert(case_name, switch_case);
            switch_items.push(case_map);
        }

        SwitchTask {
            if_: None,
            switch: switch_items,
            then: None,
            export: None,
            input: None,
            output: None,
            metadata: Map::new(),
            timeout: None,
        }
    }

    #[tokio::test]
    async fn test_switch_match_first_case() {
        // When there are 3 cases and the first case matches
        let switch_task = create_switch_task(vec![
            (
                "case1".to_string(),
                Some("${.number > 10}".to_string()),
                "case1-route".to_string(),
            ),
            (
                "case2".to_string(),
                Some("${.number > 20}".to_string()),
                "case2-route".to_string(),
            ),
            ("default".to_string(), None, "default-route".to_string()),
        ]);

        let executor = SwitchTaskExecutor::new(&switch_task);
        let workflow_context = Arc::new(RwLock::new(WorkflowContext::new_empty()));

        let input = json!({ "number": 15 });
        let task_context = TaskContext::new(
            None,
            Arc::new(input),
            Arc::new(tokio::sync::Mutex::new(serde_json::Map::new())),
        );

        let result = executor
            .execute("test_switch", workflow_context, task_context)
            .await
            .unwrap();

        // Verify that flow_directive has been changed to case1-route
        assert_eq!(
            result.flow_directive,
            Then::TaskName("case1-route".to_string())
        );
    }

    #[tokio::test]
    async fn test_switch_match_second_case() {
        // When there are 3 cases and the second case matches
        let switch_task = create_switch_task(vec![
            (
                "case1".to_string(),
                Some("${.number > 30}".to_string()),
                "case1-route".to_string(),
            ),
            (
                "case2".to_string(),
                Some("${.number > 10}".to_string()),
                "case2-route".to_string(),
            ),
            ("default".to_string(), None, "default-route".to_string()),
        ]);

        let executor = SwitchTaskExecutor::new(&switch_task);
        let workflow_context = Arc::new(RwLock::new(WorkflowContext::new_empty()));

        let input = json!({ "number": 20 });
        let task_context = TaskContext::new(
            None,
            Arc::new(input),
            Arc::new(tokio::sync::Mutex::new(serde_json::Map::new())),
        );

        let result = executor
            .execute("test_switch", workflow_context, task_context)
            .await
            .unwrap();

        // Verify that flow_directive has been changed to case2-route
        assert_eq!(
            result.flow_directive,
            Then::TaskName("case2-route".to_string())
        );
    }

    #[tokio::test]
    async fn test_switch_default_case() {
        // When no case matches, the default case is selected
        let switch_task = create_switch_task(vec![
            (
                "case1".to_string(),
                Some("${.number > 30}".to_string()),
                "case1-route".to_string(),
            ),
            (
                "case2".to_string(),
                Some("${.number > 20}".to_string()),
                "case2-route".to_string(),
            ),
            ("default".to_string(), None, "default-route".to_string()),
        ]);

        let executor = SwitchTaskExecutor::new(&switch_task);
        let workflow_context = Arc::new(RwLock::new(WorkflowContext::new_empty()));

        let input = json!({ "number": 10 });
        let task_context = TaskContext::new(
            None,
            Arc::new(input),
            Arc::new(tokio::sync::Mutex::new(serde_json::Map::new())),
        );

        let result = executor
            .execute("test_switch", workflow_context, task_context)
            .await
            .unwrap();

        // Verify that flow_directive has been changed to default-route
        assert_eq!(
            result.flow_directive,
            Then::TaskName("default-route".to_string())
        );
    }

    #[tokio::test]
    async fn test_switch_with_if_condition_true() {
        // When an if condition is specified and the condition is true, evaluation proceeds normally
        let mut switch_task = create_switch_task(vec![
            (
                "case1".to_string(),
                Some("${.number > 10}".to_string()),
                "case1-route".to_string(),
            ),
            ("default".to_string(), None, "default-route".to_string()),
        ]);

        switch_task.if_ = Some("${.enabled}".to_string());

        let executor = SwitchTaskExecutor::new(&switch_task);
        let workflow_context = Arc::new(RwLock::new(WorkflowContext::new_empty()));

        let input = json!({ "number": 15, "enabled": true });
        let task_context = TaskContext::new(
            None,
            Arc::new(input),
            Arc::new(tokio::sync::Mutex::new(serde_json::Map::new())),
        );

        let result = executor
            .execute("test_switch", workflow_context, task_context)
            .await
            .unwrap();

        // Verify that the condition matches and flow_directive has been changed to case1-route
        assert_eq!(
            result.flow_directive,
            Then::TaskName("case1-route".to_string())
        );
    }

    #[tokio::test]
    async fn test_switch_no_matches() {
        // When no case matches and there is no default case
        let switch_task = create_switch_task(vec![
            (
                "case1".to_string(),
                Some("${.number > 30}".to_string()),
                "case1-route".to_string(),
            ),
            (
                "case2".to_string(),
                Some("${.number > 20}".to_string()),
                "case2-route".to_string(),
            ),
        ]);

        let executor = SwitchTaskExecutor::new(&switch_task);
        let workflow_context = Arc::new(RwLock::new(WorkflowContext::new_empty()));

        let input = json!({ "number": 10 });
        let task_context = TaskContext::new(
            None,
            Arc::new(input),
            Arc::new(tokio::sync::Mutex::new(serde_json::Map::new())),
        );

        let result = executor
            .execute("test_switch", workflow_context, task_context)
            .await
            .unwrap();

        // Since no case matches, flow_directive remains unchanged
        assert_eq!(result.flow_directive, Then::Continue);
    }
}
