use crate::app::job::execute::UseJobExecutor;
use anyhow::Result;
use command_utils::{protobuf::ProtobufDescriptor, util::datetime};
use infra::infra::runner::rows::RunnerWithSchema;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    Priority, QueueType, ResponseType, RetryPolicy, RetryType, RunnerId, RunnerType, WorkerData,
};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    future::Future,
    hash::{DefaultHasher, Hasher},
    sync::Arc,
};
use tracing;

pub mod workflow;

pub trait McpNameConverter {
    const DELIMITER: &str = "___";
    fn combine_names(server_name: &str, tool_name: &str) -> String {
        format!("{}{}{}", server_name, Self::DELIMITER, tool_name)
    }

    fn divide_names(combined: &str) -> Option<(String, String)> {
        let delimiter = Self::DELIMITER;
        let mut v: std::collections::VecDeque<&str> = combined.split(delimiter).collect();
        match v.len().cmp(&2) {
            std::cmp::Ordering::Less => {
                tracing::error!("Failed to parse combined name: {:#?}", &combined);
                None
            }
            std::cmp::Ordering::Equal => Some((v[0].to_string(), v[1].to_string())),
            std::cmp::Ordering::Greater => {
                let server_name = v.pop_front();
                Some((
                    server_name.unwrap_or_default().to_string(),
                    v.into_iter()
                        .map(|s| s.to_string())
                        .reduce(|acc, n| format!("{acc}{delimiter}{n}"))
                        .unwrap_or_default(),
                ))
            }
        }
    }
}

pub trait FunctionCallHelper: UseJobExecutor + McpNameConverter + Send + Sync {
    const DEFAULT_RETRY_POLICY: RetryPolicy = RetryPolicy {
        r#type: RetryType::Exponential as i32,
        interval: 1000,
        max_interval: 60000,
        max_retry: 1,
        basis: 1.3,
    };

    fn timeout_sec(&self) -> u32;

    fn find_runner_by_name_with_mcp<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Future<Output = Result<Option<(RunnerWithSchema, Option<String>)>>> + Send + 'a {
        async move {
            match self.runner_app().find_runner_by_name(name).await {
                Ok(Some(runner)) => {
                    tracing::debug!("found runner: {:?}", &runner);
                    Ok(Some((runner, None)))
                }
                Ok(None) => match Self::divide_names(name) {
                    Some((server_name, tool_name)) => {
                        tracing::debug!(
                            "found calling to mcp server: {}:{}",
                            &server_name,
                            &tool_name
                        );
                        self.runner_app()
                            .find_runner_by_name(&server_name)
                            .await
                            .map(|res| res.map(|r| (r, Some(tool_name))))
                    }
                    None => Ok(None),
                },
                Err(e) => Err(e),
            }
        }
    }

    fn find_worker_by_name_with_mcp<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Future<Output = Result<Option<(WorkerData, Option<String>)>>> + Send + 'a {
        async move {
            match self.worker_app().find_by_name(name).await {
                Ok(Some(worker)) => {
                    tracing::debug!("found worker: {:?}", &worker);
                    Ok(worker.data.map(|w| (w, None)))
                }
                Ok(None) => match Self::divide_names(name) {
                    Some((server_name, tool_name)) => {
                        tracing::debug!(
                            "found calling to mcp server: {}:{}",
                            &server_name,
                            &tool_name
                        );
                        self.worker_app()
                            .find_by_name(&server_name)
                            .await
                            .map(|res| res.and_then(|r| r.data.map(|d| (d, Some(tool_name)))))
                    }
                    None => Ok(None),
                },
                Err(e) => Err(e),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_runner_call_from_llm(
        &self,
        meta: Arc<HashMap<String, String>>,
        arguments: Option<Map<String, Value>>,
        runner: RunnerWithSchema,
        tool_name_opt: Option<String>,
        worker_params: Option<serde_json::Value>,
        unique_key: Option<String>,
        timeout_sec: u32,
        streaming: bool, // TODO if true, use streaming job
    ) -> impl Future<Output = Result<Value>> + Send + '_ {
        async move {
            tracing::debug!("found runner: {:?}, tool: {:?}", &runner, &tool_name_opt);
            let (settings, arguments) = Self::prepare_runner_call_arguments(
                arguments.unwrap_or_default(),
                &runner,
                tool_name_opt,
            )
            .await?;
            if let RunnerWithSchema {
                id: Some(_id),
                data: Some(runner_data),
                ..
            } = &runner
            {
                let worker_data = self
                    .create_worker_data(&runner, settings, worker_params)
                    .await?;

                self.setup_worker_and_enqueue_with_json(
                    meta,
                    runner_data.name.as_str(),
                    worker_data,
                    arguments,
                    unique_key,
                    timeout_sec,
                    streaming,
                )
                .await
            } else {
                tracing::error!("Runner not found");
                Err(JobWorkerError::NotFound("Runner not found".to_string()).into())
            }
        }
    }

    // for LLM_CHAT, mcp proxy
    fn handle_worker_call_for_llm<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        name: &'a str,
        arguments: Option<Map<String, Value>>,
        unique_key: Option<String>,
        streaming: bool, // TODO if true, use streaming job
    ) -> impl Future<Output = Result<Value>> + Send + 'a {
        async move {
            tracing::info!("runner not found, run as worker: {:?}", &name);
            let request_args = arguments.unwrap_or_default();
            let (worker_data, tool_name_opt) = self
                .find_worker_by_name_with_mcp(name)
                .await
                .inspect_err(|e| {
                    tracing::error!("Failed to find worker: {}", e);
                })?
                .ok_or_else(|| {
                    tracing::warn!("worker not found");
                    JobWorkerError::WorkerNotFound(format!("worker or mcp tool not found: {name}"))
                })?;
            let args = if let Some(tool_name) = tool_name_opt {
                Self::correct_mcp_worker_args(tool_name, request_args.clone())?
            } else {
                request_args
            };
            self.enqueue_temp_worker_with_json(
                meta,
                &worker_data,
                Value::Object(args),
                unique_key,
                streaming,
            )
            .await
            .map(|r| r.unwrap_or_default())
        }
    }
    fn create_worker_data(
        &self,
        runner: &RunnerWithSchema,
        runner_settings: Option<serde_json::Value>,
        worker_params: Option<serde_json::Value>,
    ) -> impl Future<Output = Result<WorkerData>> + Send {
        async move {
            let settings = self
                .setup_runner_and_settings(runner, runner_settings)
                .await?;

            if let RunnerWithSchema {
                id: Some(sid),
                data: Some(sdata),
                ..
            } = runner
            {
                let worker_data =
                    Self::create_default_worker_data(*sid, &sdata.name, settings, worker_params);
                Ok(worker_data)
            } else {
                Err(JobWorkerError::InvalidParameter(format!("illegal runner: {runner:#?}")).into())
            }
        }
    }
    fn enqueue_temp_worker_with_json<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        temp_worker_data: &'a WorkerData,
        arguments: Value,
        uniq_key: Option<String>,
        _streaming: bool, // TODO if true, use streaming job
    ) -> impl Future<Output = Result<Option<Value>>> + Send + 'a {
        async move {
            let runner = if let Some(runner_id) = temp_worker_data.runner_id.as_ref() {
                self.runner_app().find_runner(runner_id).await?
            } else {
                None
            };
            if let Some(runner) = runner {
                if let Some(rdata) = runner.data.as_ref() {
                    let args_descriptor =
                        Self::parse_job_args_schema_descriptor(rdata).map_err(|e| {
                            anyhow::anyhow!("Failed to parse job_args schema descriptor: {:#?}", e)
                        })?;
                    tracing::debug!("job args: {:#?}", &arguments);
                    let job_args = if let Some(desc) = args_descriptor.clone() {
                        ProtobufDescriptor::json_value_to_message(desc, &arguments, true).map_err(
                            |e| anyhow::anyhow!("Failed to parse job_args schema: {:#?}", e),
                        )?
                    } else {
                        serde_json::to_string(&arguments)
                            .map_err(|e| anyhow::anyhow!("Failed to serialize job_args: {:#?}", e))?
                            .as_bytes()
                            .to_vec()
                    };
                    let res = self
                        .job_app()
                        .enqueue_job_with_temp_worker(
                            meta,
                            temp_worker_data.clone(),
                            job_args,
                            uniq_key,
                            0,
                            Priority::Medium as i32,
                            (self.timeout_sec() * 1000) as u64,
                            None,
                            false,
                            !temp_worker_data.use_static,
                        )
                        .await
                        .map(|res| {
                            tracing::debug!("enqueue job result: {:#?}", &res.1);
                            res.1
                        })?;
                    if let Some(r) = res {
                        Self::parse_job_result(r, rdata)
                    } else {
                        Ok(None)
                    }
                } else {
                    Err(JobWorkerError::NotFound("Runner data not found".to_string()).into())
                }
            } else {
                Err(JobWorkerError::NotFound("Runner not found".to_string()).into())
            }
        }
    }

    fn correct_mcp_worker_args(
        tool_name: String,
        arguments: Map<String, Value>,
    ) -> Result<Map<String, Value>> {
        if arguments.contains_key("tool_name") {
            if let Some(Value::String(s)) = arguments.get("tool_name") {
                if s == &tool_name {
                    return Ok(arguments);
                }
            }
            tracing::warn!(
                "tool_name is not matched: {} != {}",
                tool_name,
                arguments["tool_name"]
            );
            Err(JobWorkerError::InvalidParameter(format!(
                "tool_name is not matched: {} != {}",
                tool_name, arguments["tool_name"]
            ))
            .into())
        } else {
            // correct mcp worker args
            tracing::warn!("tool_name is not found. insert: {}", tool_name);
            let mut new_arguments = Map::new();
            new_arguments.insert("tool_name".to_string(), Value::String(tool_name));
            new_arguments.insert("arg_json".to_string(), Value::Object(arguments));
            Ok(new_arguments)
        }
    }

    fn create_default_worker_data(
        runner_id: RunnerId,
        runner_name: &str,
        runner_settings: Vec<u8>,
        worker_params: Option<serde_json::Value>,
    ) -> WorkerData {
        let mut worker: WorkerData = if let Some(serde_json::Value::Object(obj)) = worker_params {
            // override values with workflow metadata
            WorkerData {
                name: obj
                    .get("name")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| runner_name.to_string()),
                description: obj
                    .get("description")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "".to_string()),
                runner_id: Some(runner_id),
                runner_settings,
                periodic_interval: 0,
                channel: obj
                    .get("channel")
                    .and_then(|v| v.as_str().map(|s| s.to_string())),
                queue_type: obj
                    .get("queue_type")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .and_then(|s| QueueType::from_str_name(&s).map(|q| q as i32))
                    .unwrap_or(QueueType::Normal as i32),
                response_type: ResponseType::Direct as i32,
                store_success: obj
                    .get("store_success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                store_failure: obj
                    .get("store_failure")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true), //
                use_static: obj
                    .get("use_static")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                retry_policy: obj
                    .get("retry_policy")
                    .and_then(|v| v.as_object())
                    .and_then(|o| serde_json::from_value(Value::Object(o.clone())).ok()) // ignore parse errors
                    .or(Some(Self::DEFAULT_RETRY_POLICY)),
                broadcast_results: obj
                    .get("broadcast_results")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            }
        } else {
            // default values
            WorkerData {
                name: runner_name.to_string(),
                description: "Should not use other(maybe temporary)".to_string(),
                runner_id: Some(runner_id),
                runner_settings,
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: false,
                use_static: false,
                retry_policy: Some(Self::DEFAULT_RETRY_POLICY), //TODO
                broadcast_results: true,
            }
        };
        // random name (temporary name for not static worker)
        if !worker.use_static {
            let mut hasher = DefaultHasher::default();
            hasher.write_i64(datetime::now_millis());
            hasher.write_i64(rand::random()); // random
            worker.name = format!("{}_{:x}", worker.name, hasher.finish());
            tracing::debug!("Worker name with hash: {}", &worker.name);
        }
        worker
    }

    fn prepare_runner_call_arguments(
        request_args: Map<String, Value>,
        runner: &RunnerWithSchema,
        tool_name_opt: Option<String>,
    ) -> impl Future<Output = Result<(Option<Value>, Value)>> + Send + '_ {
        async move {
            let settings = request_args.get("settings").cloned();
            let arguments = if runner
                .data
                .as_ref()
                .is_some_and(|r| r.runner_type() == RunnerType::McpServer)
            {
                let mut obj_map = Map::new();
                obj_map.insert(
                    "tool_name".to_string(),
                    serde_json::to_value(tool_name_opt).map_err(|e| {
                        JobWorkerError::InvalidParameter(format!(
                            "Failed to parse tool_name: {e:?}"
                        ))
                    })?,
                );
                obj_map.insert(
                    "arg_json".to_string(),
                    Value::String(serde_json::to_string(&request_args).map_err(|e| {
                        JobWorkerError::InvalidParameter(format!(
                            "Failed to parse settings as json: {e:?}"
                        ))
                    })?),
                );
                Value::Object(obj_map)
            } else {
                request_args.get("arguments").cloned().ok_or_else(|| {
                    JobWorkerError::InvalidParameter(format!(
                        "Failed to find 'arguments' in request_args: {:#?}",
                        &request_args
                    ))
                })?
            };

            tracing::debug!(
                "runner settings: {:#?}, arguments: {:#?}",
                settings,
                arguments
            );

            Ok((settings, arguments))
        }
    }

    fn prepare_worker_call_arguments(
        request_args: Map<String, Value>,
        worker_data: &WorkerData,
        tool_name_opt: Option<String>,
    ) -> Value {
        let args = request_args
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Null);

        if worker_data.runner_id.is_some_and(|id| id.value < 0) {
            tracing::info!("worker is reusable workflow");
            serde_json::json!({
                "input": args.to_string(),
            })
        } else if let Some(tool_name) = tool_name_opt {
            serde_json::json!(
                {
                    "tool_name": tool_name,
                    "arg_json": args
                }
            )
        } else {
            args
        }
    }
}

#[cfg(test)]
mod tests {
    use memory_utils::cache::moka::{MokaCacheConfig, MokaCacheImpl};
    use proto::ProtobufHelper;

    use super::*;
    use crate::{
        app::{
            job::UseJobApp,
            job_result::UseJobResultApp,
            runner::{RunnerDataWithDescriptor, UseRunnerApp, UseRunnerParserWithCache},
            worker::UseWorkerApp,
        },
        module::test::create_hybrid_test_app,
    };
    use std::{sync::Arc, time::Duration};

    // Create a test implementation of FunctionCallHelper using the app
    struct TestFunctionCallHelper {
        app: crate::module::AppModule,
        descriptor_cache: MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>,
    }

    // Implement required traits
    impl UseJobApp for TestFunctionCallHelper {
        fn job_app(&self) -> &Arc<dyn crate::app::job::JobApp + 'static> {
            &self.app.job_app
        }
    }

    impl UseRunnerApp for TestFunctionCallHelper {
        fn runner_app(&self) -> Arc<dyn crate::app::runner::RunnerApp> {
            self.app.runner_app.clone()
        }
    }

    impl UseWorkerApp for TestFunctionCallHelper {
        fn worker_app(&self) -> &Arc<dyn crate::app::worker::WorkerApp + 'static> {
            &self.app.worker_app
        }
    }

    impl UseJobResultApp for TestFunctionCallHelper {
        fn job_result_app(&self) -> &Arc<dyn crate::app::job_result::JobResultApp + 'static> {
            &self.app.job_result_app
        }
    }
    impl UseJobExecutor for TestFunctionCallHelper {}
    impl UseRunnerParserWithCache for TestFunctionCallHelper {
        fn descriptor_cache(&self) -> &MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor> {
            &self.descriptor_cache
        }
    }

    impl ProtobufHelper for TestFunctionCallHelper {}

    impl McpNameConverter for TestFunctionCallHelper {}

    impl FunctionCallHelper for TestFunctionCallHelper {
        fn timeout_sec(&self) -> u32 {
            30 // Default timeout for tests
        }
    }

    #[test]
    fn test_function_call_helper_correct_mcp_worker_args_with_tool_name() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            // tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            // Get test app instance
            let app = create_hybrid_test_app().await.unwrap();
            let descriptor_cache = MokaCacheImpl::new(&MokaCacheConfig {
                num_counters: 10000,
                ttl: Some(Duration::from_secs(60)), // 1 minute TTL for test
            });
            let _helper = TestFunctionCallHelper {
                app,
                descriptor_cache,
            };

            // Case 1: tool_name already exists and matches the value
            let tool_name = "test_tool".to_string();
            let mut arguments = Map::new();
            arguments.insert(
                "tool_name".to_string(),
                Value::String("test_tool".to_string()),
            );
            arguments.insert("arg_json".to_string(), Value::String("value1".to_string()));

            // Using FunctionCallHelper's static method through the struct
            let result =
                TestFunctionCallHelper::correct_mcp_worker_args(tool_name, arguments.clone());
            assert!(result.is_ok());
            let result_args = result.unwrap();
            assert_eq!(result_args.len(), 2);
            assert_eq!(
                result_args["tool_name"],
                Value::String("test_tool".to_string())
            );
            assert_eq!(result_args["arg_json"], Value::String("value1".to_string()));

            // Case 2: tool_name exists but with different value
            let tool_name = "different_tool".to_string();
            let result = TestFunctionCallHelper::correct_mcp_worker_args(tool_name, arguments);
            assert!(result.is_err());
            if let Err(err) = result {
                let err_string = err.to_string();
                assert!(err_string.contains("tool_name is not matched"));
            }
        })
    }

    #[test]
    fn test_function_call_helper_correct_mcp_worker_args_without_tool_name() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            // tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            // Get test app instance
            let app = create_hybrid_test_app().await.unwrap();
            let descriptor_cache = MokaCacheImpl::new(&MokaCacheConfig {
                num_counters: 10000,
                ttl: Some(Duration::from_secs(60)), // 1 minute TTL for test
            });
            let _helper = TestFunctionCallHelper {
                app,
                descriptor_cache,
            };

            // Case 3: tool_name doesn't exist
            let tool_name = "new_tool".to_string();
            let mut arguments = Map::new();
            arguments.insert("param1".to_string(), Value::String("value1".to_string()));
            arguments.insert(
                "param2".to_string(),
                Value::Number(serde_json::Number::from(42)),
            );

            // Using FunctionCallHelper's static method through the struct
            let result =
                TestFunctionCallHelper::correct_mcp_worker_args(tool_name.clone(), arguments);
            assert!(result.is_ok());
            let result_args = result.unwrap();

            // Verify the new arguments object is constructed correctly
            assert_eq!(result_args.len(), 2);
            assert_eq!(result_args["tool_name"], Value::String(tool_name));

            // Verify that the original arguments map is stored in arg_json field
            if let Value::Object(arg_json_map) = &result_args["arg_json"] {
                assert_eq!(arg_json_map.len(), 2);
                assert_eq!(arg_json_map["param1"], Value::String("value1".to_string()));
                assert_eq!(
                    arg_json_map["param2"],
                    Value::Number(serde_json::Number::from(42))
                );
            } else {
                panic!("arg_json is not an object");
            }
        })
    }
}
