pub mod hybrid;
pub mod rdb;

use anyhow::Result;
use async_trait::async_trait;
use command_utils::protobuf::ProtobufDescriptor;
use infra::infra::runner::rows::RunnerWithSchema;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::{MokaCacheImpl, UseMokaCache};
use prost_reflect::{DynamicMessage, MessageDescriptor};
use proto::jobworkerp::data::{RunnerData, RunnerId};
use std::{fmt, future::Future, sync::Arc};

#[async_trait]
pub trait RunnerApp: fmt::Debug + Send + Sync {
    // load new runner from plugin files and store it
    async fn load_runner(&self) -> Result<bool>;

    async fn create_runner(
        &self,
        name: &str,
        description: &str,
        runner_type: i32,
        definition: &str,
    ) -> Result<RunnerId>;

    async fn delete_runner(&self, id: &RunnerId) -> Result<bool>;

    async fn find_runner(&self, id: &RunnerId) -> Result<Option<RunnerWithSchema>>
    where
        Self: Send + 'static;

    async fn find_runner_by_name(&self, name: &str) -> Result<Option<RunnerWithSchema>>
    where
        Self: Send + 'static;

    async fn find_runner_list(
        &self,
        include_full: bool,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<Vec<RunnerWithSchema>>
    where
        Self: Send + 'static;

    async fn find_runner_all_list(&self, include_full: bool) -> Result<Vec<RunnerWithSchema>>
    where
        Self: Send + 'static;

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static;

    /// Find runners with filtering and sorting (Admin UI)
    #[allow(clippy::too_many_arguments)]
    async fn find_runner_list_by(
        &self,
        runner_types: Vec<i32>,
        name_filter: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
        sort_by: Option<proto::jobworkerp::data::RunnerSortField>,
        ascending: Option<bool>,
    ) -> Result<Vec<RunnerWithSchema>>
    where
        Self: Send + 'static;

    /// Count runners with filtering (Admin UI)
    async fn count_by(&self, runner_types: Vec<i32>, name_filter: Option<String>) -> Result<i64>
    where
        Self: Send + 'static;

    /// Refresh MCP runner's sub_method_protos by re-fetching tools from MCP server
    ///
    /// # Arguments
    /// * `runner_id` - If Some, refresh only this runner. If None, refresh all MCP runners.
    ///
    /// # Returns
    /// A tuple of (updated_runner_names, failures)
    /// - updated_runner_names: List of runner names that were successfully updated
    /// - failures: List of (runner_name, error_message) pairs for failed refreshes
    async fn refresh_mcp_runner(
        &self,
        runner_id: Option<&RunnerId>,
    ) -> Result<(Vec<String>, Vec<(String, String)>)>
    where
        Self: Send + 'static;

    // for test
    #[cfg(any(test, feature = "test-utils"))]
    async fn create_test_runner(
        &self,
        runner_id: &RunnerId,
        name: &str,
    ) -> Result<RunnerDataWithDescriptor>;
}

pub trait UseRunnerApp: Send + Sync {
    fn runner_app(&self) -> Arc<dyn RunnerApp>;
}

pub trait UseRunnerParserWithCache: Send + Sync {
    fn descriptor_cache(&self) -> &MokaCacheImpl<Arc<String>, RunnerDataWithDescriptor>;

    fn _cache_key(id: &RunnerId) -> Arc<String> {
        Arc::new(format!("runner:{}", id.value))
    }

    fn clear_cache_with_descriptor(
        &self,
        runner_id: &RunnerId,
    ) -> impl std::future::Future<Output = Option<RunnerDataWithDescriptor>> + Send {
        async {
            let key = Self::_cache_key(runner_id);
            self.descriptor_cache().delete_cache(&key).await
        }
    }

    fn clear_cache_th_descriptor(&self) -> impl std::future::Future<Output = ()> + Send {
        async { self.descriptor_cache().clear().await }
    }

    /// Validate mutual exclusivity of job_args_proto and sub_method_protos
    /// per the detailed design specification (section 2.1)
    fn validate_runner_data_exclusivity(runner_data: &RunnerData) -> Result<()> {
        let has_job_args = runner_data
            .job_args_proto
            .as_ref()
            .is_some_and(|s| !s.is_empty());
        let has_sub_methods = runner_data.sub_method_protos.is_some();

        if has_sub_methods && has_job_args {
            return Err(JobWorkerError::InvalidParameter(
                "job_args_proto and sub_method_protos cannot both be set".to_string(),
            )
            .into());
        }

        Ok(())
    }

    // TODO remove if not used
    fn parse_proto_schemas(&self, runner_data: RunnerData) -> Result<RunnerDataWithDescriptor> {
        // Validate mutual exclusivity of job_args_proto and sub_method_protos
        Self::validate_runner_data_exclusivity(&runner_data)?;

        // runner_settings_proto
        let ope_d = if runner_data.runner_settings_proto.is_empty() {
            None
        } else {
            let ope_d =
                ProtobufDescriptor::new(&runner_data.runner_settings_proto).map_err(|e| {
                    JobWorkerError::ParseError(format!("schema runner_settings_proto error:{e:?}"))
                })?;
            let _ope_m = ope_d
                .get_messages()
                .first()
                .ok_or(JobWorkerError::InvalidParameter(format!(
                    "illegal RunnerData: message name is not found from {}",
                    runner_data.runner_settings_proto
                )))?;
            Some(ope_d)
        };
        // job_args_proto
        let arg_d = match &runner_data.job_args_proto {
            Some(job_args_proto) if !job_args_proto.is_empty() => {
                let arg_d = ProtobufDescriptor::new(job_args_proto).map_err(|e| {
                    JobWorkerError::ParseError(format!("schema job_args_proto error:{e:?}"))
                })?;
                let _arg_m =
                    arg_d
                        .get_messages()
                        .first()
                        .ok_or(JobWorkerError::InvalidParameter(format!(
                            "illegal RunnerData: message name is not found from {}",
                            job_args_proto
                        )))?;
                Some(arg_d)
            }
            _ => None, // use JobResult as job_args_proto
        };
        // result_output_proto
        let result_d = if let Some(result_output_proto) = &runner_data.result_output_proto {
            if (*result_output_proto).is_empty() {
                None
            } else {
                let result_d = ProtobufDescriptor::new(result_output_proto).map_err(|e| {
                    JobWorkerError::ParseError(format!("schema result_output_proto error:{e:?}"))
                })?;
                let _result_m =
                    result_d
                        .get_messages()
                        .first()
                        .ok_or(JobWorkerError::InvalidParameter(format!(
                        "illegal RunnerData: message name is not found from {result_output_proto}"
                    )))?;
                Some(result_d)
            }
        } else {
            None
        };
        Ok(RunnerDataWithDescriptor {
            runner_data,
            runner_settings_descriptor: ope_d,
            args_descriptor: arg_d,
            result_descriptor: result_d,
        })
    }

    fn store_proto_cache(
        &self,
        runner_id: &RunnerId,
        runner_with_descriptor: &RunnerDataWithDescriptor,
    ) -> impl std::future::Future<Output = ()> + Send {
        async {
            let key = Self::_cache_key(runner_id);
            self.descriptor_cache()
                .set_cache(key, runner_with_descriptor.clone())
                .await
        }
    }
    fn parse_proto_with_cache(
        &self,
        runner_id: &RunnerId,
        runner_data: &RunnerData,
    ) -> impl Future<Output = Result<RunnerDataWithDescriptor>> + Send {
        async {
            let key = Self::_cache_key(runner_id);
            self.descriptor_cache()
                .with_cache(&key, || async {
                    self.parse_proto_schemas(runner_data.clone())
                })
                .await
        }
    }
    fn validate_runner_settings_data_with_schema(
        &self,
        runner_id: &RunnerId,
        runner_data: &RunnerData,
        runner_settings: &[u8],
    ) -> impl Future<Output = Result<Option<RunnerDataWithDescriptor>>> + Send {
        async move {
            let runner_with_descriptor =
                self.parse_proto_with_cache(runner_id, runner_data).await?;
            runner_with_descriptor.parse_runner_settings_data(runner_settings)?;
            Ok(Some(runner_with_descriptor))
        }
    }
}
pub trait UseRunnerAppParserWithCache:
    UseRunnerApp + UseRunnerParserWithCache + Send + Sync
{
    fn validate_runner_settings_data(
        &self,
        runner_id: &RunnerId,
        runner_settings: &[u8],
    ) -> impl Future<Output = Result<Option<RunnerDataWithDescriptor>>> + Send {
        let runner_app = self.runner_app().clone();
        async move {
            if let Some(RunnerWithSchema {
                id: _,
                data: Some(runner_data),
                ..
            }) = runner_app.find_runner(runner_id).await?
            {
                self.validate_runner_settings_data_with_schema(
                    runner_id,
                    &runner_data,
                    runner_settings,
                )
                .await
            } else {
                Err(JobWorkerError::InvalidParameter(format!(
                    "illegal RunnerData: runner is not found: id={}",
                    runner_id.value
                ))
                .into())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunnerDataWithDescriptor {
    pub runner_data: RunnerData,
    pub runner_settings_descriptor: Option<ProtobufDescriptor>,
    pub args_descriptor: Option<ProtobufDescriptor>,
    pub result_descriptor: Option<ProtobufDescriptor>,
}
impl RunnerDataWithDescriptor {
    pub fn get_runner_settings_message(&self) -> Result<Option<MessageDescriptor>> {
        if let Some(op) = &self.runner_settings_descriptor {
            op.get_messages()
                .first()
                .cloned()
                .ok_or(
                    JobWorkerError::InvalidParameter(format!(
                        "illegal RunnerData: runner_settings message is not found from:\n {}",
                        &self.runner_data.runner_settings_proto
                    ))
                    .into(),
                )
                .map(Some)
        } else {
            Ok(None)
        }
    }
    pub fn parse_runner_settings_data(
        &self,
        runner_settings: &[u8],
    ) -> Result<Option<DynamicMessage>> {
        if let Some(op) = &self.runner_settings_descriptor {
            self.get_runner_settings_message()?
                .ok_or(
                    JobWorkerError::InvalidParameter(format!(
                        "illegal RunnerData: runner_settings message is not found from:\n {}",
                        &self.runner_data.runner_settings_proto
                    ))
                    .into(),
                )
                .and_then(|m| {
                    op.get_message_by_name_from_bytes(m.full_name(), runner_settings)
                        .map(Some)
                        .map_err(|e| {
                            JobWorkerError::InvalidParameter(format!(
                                "illegal runner_settings data: cannot parse runner_settings data as {}: {:?}",
                                m.full_name(),
                                e
                            ))
                            .into()
                        })
                })
        } else {
            Ok(None)
        }
    }
    pub fn get_job_args_message(&self) -> Result<Option<MessageDescriptor>> {
        if let Some(op) = &self.args_descriptor {
            op.get_messages()
                .first()
                .cloned()
                .ok_or(
                    JobWorkerError::InvalidParameter(format!(
                        "illegal RunnerData: job args message name is not found from:\n {:?}",
                        &self.runner_data.job_args_proto
                    ))
                    .into(),
                )
                .map(Some)
        } else {
            Ok(None)
        }
    }
}

pub trait RunnerCacheHelper {
    fn find_cache_key(id: &i64) -> Arc<String> {
        Arc::new(["runner_id:", &id.to_string()].join(""))
    }

    fn find_name_cache_key(name: &str) -> Arc<String> {
        Arc::new(["runner_name:", name].join(""))
    }

    fn find_all_list_cache_key(include_full: bool) -> Arc<String> {
        Arc::new(format!(
            "runner_list:all{}",
            if include_full { ":full" } else { "" }
        ))
    }
    // XXX cannot expire properly (should make it hash key?)
    fn find_list_cache_key(
        include_full: bool,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Arc<String> {
        if limit.is_none() && offset.is_none() {
            Self::find_all_list_cache_key(include_full)
        } else {
            Arc::new(format!(
                "runner_list{}:{}-{}",
                if include_full { ":full" } else { "" },
                limit.map_or("none".to_string(), |l| l.to_string()),
                offset.map_or("0".to_string(), |o| o.to_string())
            ))
        }
    }
}

// #[cfg(test)]
#[cfg(any(test, feature = "test-utils"))]
pub mod test {
    use std::vec;

    use super::RunnerDataWithDescriptor;
    use infra::infra::runner::rows::RunnerWithSchema;
    use proto::jobworkerp::data::{RunnerData, RunnerId, RunnerType, StreamingOutputType};
    pub fn test_runner_data(name: &str) -> RunnerData {
        proto::jobworkerp::data::RunnerData {
            name: name.to_string(),
            description: "test runner desc".to_string(),
            runner_settings_proto: include_str!("../../../proto/protobuf/test_runner.proto")
                .to_string(),
            job_args_proto: Some(
                include_str!("../../../proto/protobuf/test_args.proto").to_string(),
            ),
            runner_type: RunnerType::Plugin as i32,
            result_output_proto: None,
            output_type: StreamingOutputType::NonStreaming as i32,
            definition: "./target/debug/libplugin_runner_test.so".to_string(),
            sub_method_protos: None,
        }
    }
    pub fn test_runner_with_schema(id: &RunnerId, name: &str) -> RunnerWithSchema {
        RunnerWithSchema {
            id: Some(*id),
            data: Some(test_runner_data(name)),
            settings_schema: "settings_schema".to_string(),
            arguments_schema: "arguments_schema".to_string(),
            output_schema: Some("output_schema".to_string()),
            tools: vec![],
        }
    }

    pub fn test_runner_with_descriptor(name: &str) -> RunnerDataWithDescriptor {
        let runner_data = test_runner_data(name);
        RunnerDataWithDescriptor {
            runner_data: runner_data.clone(),
            runner_settings_descriptor: Some(
                command_utils::protobuf::ProtobufDescriptor::new(
                    &runner_data.runner_settings_proto,
                )
                .unwrap(),
            ),
            args_descriptor: runner_data
                .job_args_proto
                .as_ref()
                .map(|proto| command_utils::protobuf::ProtobufDescriptor::new(proto).unwrap()),
            result_descriptor: None,
        }
    }
}
