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
    /// 新しいSwitchTaskExecutorを作成する
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
    /// Switchタスクを実行する
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
            // 各ケースは HashMap<String, SwitchCase> の形式で1つの要素のみ含む
            for (case_name, switch_case) in switch_item.iter() {
                // when condition
                let matched_case = if let Some(when) = &switch_case.when {
                    match Self::execute_transform_as_bool(
                        task_context.raw_input.clone(),
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

                    task_context.flow_directive = Then::from(switch_case.then.clone());
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
    use crate::workflow::definition::workflow::{
        FlowDirective, FlowDirectiveEnum, SwitchCase, SwitchTask,
    };
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
            let flow_directive = FlowDirective {
                subtype_0: Some(FlowDirectiveEnum::Continue),
                subtype_1: Some(then_value),
            };

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
        // 3つのcaseがあり、最初のcaseがマッチする場合
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

        // flow_directiveがcase1-routeに変更されたことを検証する
        assert_eq!(
            result.flow_directive,
            Then::TaskName("case1-route".to_string())
        );
    }

    #[tokio::test]
    async fn test_switch_match_second_case() {
        // 3つのcaseがあり、2番目のcaseがマッチする場合
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

        // flow_directiveがcase2-routeに変更されたことを検証する
        assert_eq!(
            result.flow_directive,
            Then::TaskName("case2-route".to_string())
        );
    }

    #[tokio::test]
    async fn test_switch_default_case() {
        // どのcaseもマッチしない場合、デフォルトケースが選択される
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

        // flow_directiveがdefault-routeに変更されたことを検証する
        assert_eq!(
            result.flow_directive,
            Then::TaskName("default-route".to_string())
        );
    }

    #[tokio::test]
    async fn test_switch_with_if_condition_true() {
        // ifが指定されている場合で条件がtrueの場合は通常通り評価される
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

        // 条件がマッチしてcase1-routeに変更されたことを検証
        assert_eq!(
            result.flow_directive,
            Then::TaskName("case1-route".to_string())
        );
    }

    #[tokio::test]
    async fn test_switch_no_matches() {
        // どのケースもマッチせず、デフォルトケースもない場合
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

        // どのケースもマッチしないため、flow_directiveは変更されない
        assert_eq!(result.flow_directive, Then::Continue);
    }
}
