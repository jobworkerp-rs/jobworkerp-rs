// use super::run::RunTaskExecutor;
// use super::TaskExecutorTrait;
// use crate::workflow::definition::workflow::{
//     self, RetryLimit, RetryLimitAttempt, RunTaskConfiguration,
// };
// use crate::workflow::definition::workflow::{FunctionOptions, RunTask};
// use crate::workflow::definition::UseLoadUrlOrPath;
// use crate::workflow::execute::context::{TaskContext, WorkflowContext};
// use crate::workflow::execute::job::JobExecutorWrapper;
// use anyhow::Result;
// use memory_utils::lock::RwLockWithKey;
// use memory_utils::cache::stretto::{MemoryCacheConfig, UseMemoryCache};
// use net_utils::net::reqwest;
// use serde_json::Map;
// use std::sync::Arc;
// use std::time::Duration;
// use stretto::AsyncCache;
// use tokio::sync::RwLock;

// pub struct CallTaskExecutor {
//     task: workflow::CallTask,
//     http_client: reqwest::ReqwestClient,
//     job_executor_wrapper: Arc<JobExecutorWrapper>,
//     memory_cache: memory_utils::cache::stretto::MemoryCacheImpl<String, workflow::RunTask>,
// }
// impl CallTaskExecutor {
//     const TIMEOUT_SEC: u64 = 300; // 5 minutes
//     const FUNCTION_OPTIONS_METADATA_LABEL: &'static str = "options";
//     const RUNNER_SETTINGS_METADATA_LABEL: &'static str = "settings";
//     pub fn new(
//         task: workflow::CallTask,
//         http_client: reqwest::ReqwestClient,
//         job_executor_wrapper: Arc<JobExecutorWrapper>,
//     ) -> Self {
//         Self {
//             task,
//             http_client,
//             job_executor_wrapper,
//             memory_cache: memory_utils::cache::stretto::MemoryCacheImpl::new(
//                 &MemoryCacheConfig::default(),
//                 Some(Duration::from_secs(Self::TIMEOUT_SEC)),
//             ),
//         }
//     }
//     // Apply metadata json values to FunctionOptions
//     fn apply_options_from_json(
//         options: &mut FunctionOptions,
//         options_map: &serde_json::Map<String, serde_json::Value>,
//     ) {
//         if let Some(serde_json::Value::Bool(broadcast)) = options_map.get("broadcastResults") {
//             options.broadcast_results = Some(*broadcast);
//         }

//         if let Some(serde_json::Value::String(channel)) = options_map.get("channel") {
//             options.channel = Some(channel.clone());
//         }

//         if let Some(serde_json::Value::Object(retry_options)) = options_map.get("retry") {
//             // Clone existing retry_policy if present or create new one
//             let mut retry = options.retry.clone().unwrap_or_default();

//             // Update retry policy fields from metadata
//             if let Some(serde_json::Value::String(interval)) = retry_options.get("backoff") {
//                 match interval.as_str() {
//                     "exponential" => {
//                         retry.backoff = Some(workflow::RetryBackoff::Exponential(Map::default()));
//                     }
//                     "linear" => {
//                         retry.backoff = Some(workflow::RetryBackoff::Linear(Map::default()));
//                     }
//                     "constant" => {
//                         retry.backoff = Some(workflow::RetryBackoff::Constant(Map::default()));
//                     }
//                     _ => {
//                         retry.backoff = None;
//                     }
//                 };
//             }

//             // Handle limit.attempt fields
//             if let Some(serde_json::Value::Object(limit_options)) = retry_options.get("limit") {
//                 if let Some(serde_json::Value::Object(attempt_options)) =
//                     limit_options.get("attempt")
//                 {
//                     let mut retry_attempt = RetryLimitAttempt::default();

//                     if let Some(serde_json::Value::Number(count)) = attempt_options.get("count") {
//                         if let Some(count_val) = count.as_i64() {
//                             retry_attempt.count = Some(count_val);
//                         }
//                     }

//                     if let Some(serde_json::Value::Number(duration)) =
//                         attempt_options.get("duration")
//                     {
//                         if let Some(duration_val) = duration.as_i64() {
//                             retry_attempt.duration =
//                                 Some(workflow::Duration::from_millis(duration_val as u64));
//                         }
//                     }

//                     if retry_attempt != RetryLimitAttempt::default() {
//                         retry.limit = Some(RetryLimit {
//                             attempt: Some(retry_attempt),
//                         });
//                     }
//                 }
//             }

//             if let Some(serde_json::Value::Number(delay)) = retry_options.get("delay") {
//                 retry.delay = delay.as_u64().map(workflow::Duration::from_millis);
//             }

//             options.retry = Some(retry);
//         }

//         if let Some(serde_json::Value::Bool(store_failure)) = options_map.get("storeFailure") {
//             options.store_failure = Some(*store_failure);
//         }

//         if let Some(serde_json::Value::Bool(store_success)) = options_map.get("storeSuccess") {
//             options.store_success = Some(*store_success);
//         }

//         if let Some(serde_json::Value::Bool(use_static)) = options_map.get("useStatic") {
//             options.use_static = Some(*use_static);
//         }

//         if let Some(serde_json::Value::Bool(with_backup)) = options_map.get("withBackup") {
//             options.with_backup = Some(*with_backup);
//         }
//     }

//     // overwrite settings, options values with metadata if exists and return overwritten objects
//     fn extract_metadata(
//         metadata: &serde_json::Map<String, serde_json::Value>,
//         mut settings: serde_json::Map<String, serde_json::Value>,
//         options: Option<FunctionOptions>,
//     ) -> (
//         Option<FunctionOptions>,
//         serde_json::Map<String, serde_json::Value>,
//     ) {
//         if let Some(serde_json::Value::Object(settings_map)) =
//             metadata.get(Self::RUNNER_SETTINGS_METADATA_LABEL)
//         {
//             command_utils::util::json::merge_obj(&mut settings, settings_map.clone());
//         }
//         if let Some(serde_json::Value::Object(options_map)) =
//             metadata.get(Self::FUNCTION_OPTIONS_METADATA_LABEL)
//         {
//             let mut options = options.unwrap_or_default();
//             Self::apply_options_from_json(&mut options, options_map);
//             (Some(options), settings)
//         } else {
//             (options, settings)
//         }
//     }
// }
// impl UseMemoryCache<String, workflow::RunTask> for CallTaskExecutor {
//     fn cache(&self) -> &AsyncCache<std::string::String, workflow::RunTask> {
//         self.memory_cache.cache()
//     }

//     #[doc = " default cache ttl"]
//     fn default_ttl(&self) -> Option<&Duration> {
//         self.memory_cache.default_ttl()
//     }

//     fn key_lock(&self) -> &RwLockWithKey<String> {
//         self.memory_cache.key_lock()
//     }
// }
// impl UseLoadUrlOrPath for CallTaskExecutor {
//     fn http_client(&self) -> &infra_utils::infra::net::reqwest::ReqwestClient {
//         &self.http_client
//     }
// }
// impl TaskExecutorTrait<'_> for CallTaskExecutor {
//     async fn execute(
//         &self,
//         task_name: &str,
//         workflow_context: Arc<RwLock<WorkflowContext>>,
//         mut task_context: TaskContext,
//     ) -> Result<TaskContext, Box<workflow::Error>> {
//         let workflow::CallTask {
//             // TODO: add other task types
//             call,
//             export: _export,
//             if_: _if_,
//             input: _input,
//             metadata: call_metadata,
//             output: call_output,
//             then: _then,
//             timeout: _timeout,
//             with, // TODO return as argument for return value
//         } = &self.task;
//         {
//             // enter call position
//             task_context.add_position_name("call".to_string());

//             // TODO name reference (inner yaml, public catalog)
//             let fun = match self
//                 .with_cache_if_some(call, None, || self.load_url_or_path(call.as_str()))
//                 .await
//             {
//                 Ok(Some(fun)) => fun,
//                 Ok(None) => {
//                     let title = format!("Call task not found: {}", call.as_str());
//                     tracing::error!("{}", title);
//                     let pos = task_context.position.clone();
//                     return Err(workflow::errors::ErrorFactory::new().not_found(
//                         title,
//                         Some(&pos),
//                         None,
//                     ));
//                 }
//                 Err(e) => {
//                     let title = format!("Failed to load call task: {}", e);
//                     tracing::error!("{}", title);
//                     let pos = task_context.position.clone();
//                     return Err(workflow::errors::ErrorFactory::new().not_found(
//                         title,
//                         Some(&pos),
//                         None,
//                     ));
//                 }
//             };
//             // TODO: add other task types
//             let RunTask {
//                 export,
//                 if_,
//                 input,
//                 metadata,
//                 output,
//                 run:
//                     RunTaskConfiguration {
//                         await_,
//                         function:
//                             workflow::Function {
//                                 mut arguments,
//                                 options,
//                                 runner_name,
//                                 settings: loaded_settings,
//                             },
//                         return_,
//                     },
//                 then,
//                 timeout,
//             } = fun;
//             {
//                 let (options, settings) =
//                     Self::extract_metadata(call_metadata, loaded_settings, options);
//                 let task = RunTask {
//                     export,
//                     if_,
//                     input,
//                     metadata,
//                     output: call_output.clone().or(output), // TODO merge output
//                     // XXX 1 pattern only
//                     run: RunTaskConfiguration {
//                         await_,
//                         function: workflow::Function {
//                             arguments: {
//                                 command_utils::util::json::merge_obj(&mut arguments, with.clone());
//                                 arguments
//                             },
//                             options,
//                             runner_name,
//                             settings,
//                         },
//                         return_,
//                     },
//                     then,
//                     timeout,
//                 };
//                 let executor = RunTaskExecutor::new(self.job_executor_wrapper.clone(), task);
//                 let res = executor
//                     .execute(task_name, workflow_context, task_context)
//                     .await;
//                 match res {
//                     Ok(mut task_context) => {
//                         // remove call position
//                         task_context.remove_position();
//                         Ok(task_context)
//                     }
//                     Err(e) => Err(e),
//                 }
//             } // _ => {
//               //     tracing::error!("not supported the called function for now: {:?}", call);
//               //     Err(anyhow!(
//               //         "not supported the called function for now: {:?}",
//               //         call
//               //     ))
//               // }
//         } // _ => {
//           //     tracing::error!("not supported the call for now: {:?}", &self.task);
//           //     Err(anyhow!("not supported the call for now: {:?}", &self.task))
//           // }
//     }
// }

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use serde_json::json;

//     #[test]
//     fn test_extract_metadata_with_settings_and_options() {
//         // Arrange
//         let metadata =
//             serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(json!({
//                 "settings": {
//                     "setting1": "value1",
//                     "setting2": 42
//                 },
//                 "options": {
//                     "broadcastResults": true,
//                     "channel": "test-channel",
//                     "storeFailure": true,
//                     "storeSuccess": false,
//                     "useStatic": true,
//                     "withBackup": false,
//                     "retry": {
//                         "backoff": "exponential",
//                         "limit": {
//                             "attempt": {
//                                 "count": 5,
//                                 "duration": 1000
//                             }
//                         },
//                         "delay": 100
//                     }
//                 }
//             }))
//             .unwrap();

//         let settings =
//             serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(json!({
//                 "existingSetting": "value",
//                 "setting1": "original" // This should be overwritten
//             }))
//             .unwrap();

//         let options = None; // Test creating new options

//         // Act
//         let (result_options, result_settings) =
//             CallTaskExecutor::extract_metadata(&metadata, settings, options);

//         // Assert
//         // Verify settings were merged properly
//         assert_eq!(
//             result_settings
//                 .get("existingSetting")
//                 .unwrap()
//                 .as_str()
//                 .unwrap(),
//             "value"
//         );
//         assert_eq!(
//             result_settings.get("setting1").unwrap().as_str().unwrap(),
//             "value1"
//         ); // Overwritten
//         assert_eq!(
//             result_settings.get("setting2").unwrap().as_i64().unwrap(),
//             42
//         ); // Added

//         // Verify options were created and populated correctly
//         let options = result_options.unwrap();
//         assert_eq!(options.broadcast_results, Some(true));
//         assert_eq!(options.channel, Some("test-channel".to_string()));
//         assert_eq!(options.store_failure, Some(true));
//         assert_eq!(options.store_success, Some(false));
//         assert_eq!(options.use_static, Some(true));
//         assert_eq!(options.with_backup, Some(false));

//         // Verify retry policy
//         let retry = options.retry.unwrap();
//         match retry.backoff.unwrap() {
//             workflow::RetryBackoff::Exponential(_) => {} // Success
//             _ => panic!("Expected exponential backoff"),
//         }
//         let limit = retry.limit.unwrap();
//         let attempt = limit.attempt.unwrap();
//         assert_eq!(attempt.count, Some(5));
//         assert_eq!(attempt.duration.unwrap().to_millis(), 1000);
//         assert_eq!(retry.delay.unwrap().to_millis(), 100);
//     }

//     #[test]
//     fn test_extract_metadata_with_existing_options() {
//         // Arrange
//         let metadata =
//             serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(json!({
//                 "options": {
//                     "broadcastResults": true,
//                     "channel": "test-channel"
//                 }
//             }))
//             .unwrap();

//         let settings = serde_json::Map::new();

//         let original_options = FunctionOptions {
//             store_success: Some(true),
//             store_failure: Some(false),
//             use_static: Some(false),
//             with_backup: Some(false),
//             ..Default::default()
//         };

//         // Act
//         let (result_options, _) =
//             CallTaskExecutor::extract_metadata(&metadata, settings, Some(original_options));

//         // Assert
//         let options = result_options.unwrap();
//         assert_eq!(options.broadcast_results, Some(true)); // From metadata
//         assert_eq!(options.channel, Some("test-channel".to_string())); // From metadata
//         assert_eq!(options.store_success, Some(true)); // Preserved from original
//         assert_eq!(options.store_failure, Some(false)); // Preserved from original
//     }

//     #[test]
//     fn test_extract_metadata_no_metadata() {
//         // Arrange
//         let metadata = serde_json::Map::new();
//         let settings = serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(
//             json!({"setting1": "value1"}),
//         )
//         .unwrap();

//         let options = FunctionOptions {
//             store_success: Some(true),
//             ..Default::default()
//         };

//         // Act
//         let (result_options, result_settings) =
//             CallTaskExecutor::extract_metadata(&metadata, settings, Some(options));

//         // Assert
//         assert_eq!(
//             result_settings.get("setting1").unwrap().as_str().unwrap(),
//             "value1"
//         );
//         let options = result_options.unwrap();
//         assert_eq!(options.store_success, Some(true)); // Original value preserved
//         assert_eq!(options.broadcast_results, None); // No metadata to apply
//     }
// }
