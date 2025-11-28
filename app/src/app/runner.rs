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
use std::{collections::HashMap, fmt, future::Future, sync::Arc};

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

    /// Phase 6.6.4: Validate that method_proto_map is present (now required for all runners)
    fn validate_runner_data_has_method_proto_map(runner_data: &RunnerData) -> Result<()> {
        if runner_data.method_proto_map.is_none() {
            return Err(JobWorkerError::InvalidParameter(
                "method_proto_map is required for all runners (Phase 6.6.4+)".to_string(),
            )
            .into());
        }
        Ok(())
    }

    /// Parse proto schemas from RunnerData and generate Protobuf descriptors
    ///
    /// Phase 6.6.7: Complete implementation - generates descriptors for all methods
    fn parse_proto_schemas(&self, runner_data: RunnerData) -> Result<RunnerDataWithDescriptor> {
        // Phase 6.6.4: Validate that method_proto_map is present (required)
        Self::validate_runner_data_has_method_proto_map(&runner_data)?;

        // Parse runner_settings_proto
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

        // Phase 6.6.7: Parse method_proto_map and generate descriptors for each method
        let mut method_descriptors = HashMap::new();

        if let Some(method_proto_map) = &runner_data.method_proto_map {
            for (method_name, method_schema) in &method_proto_map.schemas {
                // Parse args_proto
                let args_desc = if !method_schema.args_proto.is_empty() {
                    Some(
                        ProtobufDescriptor::new(&method_schema.args_proto).map_err(|e| {
                            JobWorkerError::ParseError(format!(
                                "Failed to parse args_proto for method '{}': {:?}",
                                method_name, e
                            ))
                        })?,
                    )
                } else {
                    None
                };

                // Parse result_proto (may be empty for unstructured output)
                let result_desc = if !method_schema.result_proto.is_empty() {
                    Some(
                        ProtobufDescriptor::new(&method_schema.result_proto).map_err(|e| {
                            JobWorkerError::ParseError(format!(
                                "Failed to parse result_proto for method '{}': {:?}",
                                method_name, e
                            ))
                        })?,
                    )
                } else {
                    tracing::warn!(
                        "Runner '{}' method '{}' has empty result_proto (unstructured output)",
                        runner_data.name,
                        method_name
                    );
                    None
                };

                method_descriptors.insert(
                    method_name.clone(),
                    MethodDescriptors {
                        args_descriptor: args_desc,
                        result_descriptor: result_desc,
                    },
                );
            }
        }

        // Backward compatibility: Populate legacy fields with "run" method's descriptors
        let (arg_d, result_d) = method_descriptors
            .get("run")
            .map(|desc| (desc.args_descriptor.clone(), desc.result_descriptor.clone()))
            .unwrap_or((None, None));

        Ok(RunnerDataWithDescriptor {
            runner_data,
            runner_settings_descriptor: ope_d,
            method_descriptors,
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

/// Holds protobuf descriptors for a single method's input and output schemas
#[derive(Debug, Clone)]
pub struct MethodDescriptors {
    pub args_descriptor: Option<ProtobufDescriptor>,
    pub result_descriptor: Option<ProtobufDescriptor>,
}

/// Runner data with cached protobuf descriptors
///
/// Phase 6.6.7: Extended to support method-level descriptors for multi-method runners (MCP/Plugin)
#[derive(Debug, Clone)]
pub struct RunnerDataWithDescriptor {
    pub runner_data: RunnerData,
    pub runner_settings_descriptor: Option<ProtobufDescriptor>,

    // Phase 6.6.7: Method-level descriptor cache
    // - Key: method name (e.g., "run", "fetch_html", "get_current_time")
    // - Value: MethodDescriptors (args_descriptor, result_descriptor)
    // - For single-method runners: contains one entry with key "run"
    // - For MCP/Plugin runners: contains multiple entries for each tool/method
    pub method_descriptors: HashMap<String, MethodDescriptors>,

    // Deprecated: Use method_descriptors instead
    // Kept for backward compatibility - populated with "run" method's descriptors for single-method runners
    pub args_descriptor: Option<ProtobufDescriptor>,
    pub result_descriptor: Option<ProtobufDescriptor>,
}
impl RunnerDataWithDescriptor {
    /// Parse proto schemas from RunnerData (public static method for testing)
    ///
    /// Phase 6.6.7.7: Public method for unit testing without trait requirement
    pub fn parse_proto_schemas_from_runner_data(
        runner_data: RunnerData,
    ) -> Result<RunnerDataWithDescriptor> {
        // Phase 6.6.4: Validate that method_proto_map is present (required)
        if runner_data.method_proto_map.is_none() {
            return Err(JobWorkerError::InvalidParameter(
                "method_proto_map is required for all runners".to_string(),
            )
            .into());
        }

        // Parse runner_settings_proto
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

        // Phase 6.6.7: Parse method_proto_map and generate descriptors for each method
        let mut method_descriptors = HashMap::new();

        if let Some(method_proto_map) = &runner_data.method_proto_map {
            for (method_name, method_schema) in &method_proto_map.schemas {
                // Parse args_proto
                let args_desc = if !method_schema.args_proto.is_empty() {
                    Some(
                        ProtobufDescriptor::new(&method_schema.args_proto).map_err(|e| {
                            JobWorkerError::ParseError(format!(
                                "Failed to parse args_proto for method '{}': {:?}",
                                method_name, e
                            ))
                        })?,
                    )
                } else {
                    None
                };

                // Parse result_proto (may be empty for unstructured output)
                let result_desc = if !method_schema.result_proto.is_empty() {
                    Some(
                        ProtobufDescriptor::new(&method_schema.result_proto).map_err(|e| {
                            JobWorkerError::ParseError(format!(
                                "Failed to parse result_proto for method '{}': {:?}",
                                method_name, e
                            ))
                        })?,
                    )
                } else {
                    None
                };

                method_descriptors.insert(
                    method_name.clone(),
                    MethodDescriptors {
                        args_descriptor: args_desc,
                        result_descriptor: result_desc,
                    },
                );
            }
        }

        // Backward compatibility: Populate legacy fields with "run" method's descriptors
        let (arg_d, result_d) = method_descriptors
            .get("run")
            .map(|desc| (desc.args_descriptor.clone(), desc.result_descriptor.clone()))
            .unwrap_or((None, None));

        Ok(RunnerDataWithDescriptor {
            runner_data,
            runner_settings_descriptor: ope_d,
            method_descriptors,
            args_descriptor: arg_d,
            result_descriptor: result_d,
        })
    }

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
                    JobWorkerError::InvalidParameter(
                        "illegal RunnerData: job args message name is not found from method_proto_map"
                            .to_string(),
                    )
                    .into(),
                )
                .map(Some)
        } else {
            Ok(None)
        }
    }

    // Phase 6.6.7: Method-level descriptor access APIs

    /// Get args descriptor for a specific method (using)
    ///
    /// # Arguments
    /// * `using` - Optional method name. If None, uses default method "run"
    ///
    /// # Returns
    /// Option<&ProtobufDescriptor> for the specified method's arguments
    pub fn get_args_descriptor_for_method(
        &self,
        using: Option<&str>,
    ) -> Option<&ProtobufDescriptor> {
        let method_name = using.unwrap_or("run");
        self.method_descriptors
            .get(method_name)
            .and_then(|desc| desc.args_descriptor.as_ref())
    }

    /// Get result descriptor for a specific method (using)
    ///
    /// # Arguments
    /// * `using` - Optional method name. If None, uses default method "run"
    ///
    /// # Returns
    /// Option<&ProtobufDescriptor> for the specified method's result
    pub fn get_result_descriptor_for_method(
        &self,
        using: Option<&str>,
    ) -> Option<&ProtobufDescriptor> {
        let method_name = using.unwrap_or("run");
        self.method_descriptors
            .get(method_name)
            .and_then(|desc| desc.result_descriptor.as_ref())
    }

    /// Get args MessageDescriptor for a specific method
    ///
    /// # Arguments
    /// * `using` - Optional method name. If None, uses default method "run"
    ///
    /// # Returns
    /// Result<Option<MessageDescriptor>> - Some if method exists and has args schema, None if no schema
    pub fn get_job_args_message_for_method(
        &self,
        using: Option<&str>,
    ) -> Result<Option<MessageDescriptor>> {
        let method_name = using.unwrap_or("run");
        if let Some(descriptor) = self.get_args_descriptor_for_method(using) {
            descriptor
                .get_messages()
                .first()
                .cloned()
                .ok_or_else(|| {
                    JobWorkerError::InvalidParameter(format!(
                        "illegal RunnerData: job args message is not found for method '{}'",
                        method_name
                    ))
                    .into()
                })
                .map(Some)
        } else {
            Ok(None)
        }
    }

    /// Get result MessageDescriptor for a specific method
    ///
    /// # Arguments
    /// * `using` - Optional method name. If None, uses default method "run"
    ///
    /// # Returns
    /// Result<Option<MessageDescriptor>> - Some if method exists and has result schema, None if no schema
    pub fn get_job_result_message_for_method(
        &self,
        using: Option<&str>,
    ) -> Result<Option<MessageDescriptor>> {
        let method_name = using.unwrap_or("run");
        if let Some(descriptor) = self.get_result_descriptor_for_method(using) {
            descriptor
                .get_messages()
                .first()
                .cloned()
                .ok_or_else(|| {
                    JobWorkerError::InvalidParameter(format!(
                        "illegal RunnerData: job result message is not found for method '{}'",
                        method_name
                    ))
                    .into()
                })
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
        // Phase 6.6.4: Use method_proto_map (required for all runners)
        let mut schemas = std::collections::HashMap::new();
        schemas.insert(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../../proto/protobuf/test_args.proto").to_string(),
                result_proto: String::new(),
                description: Some("Test runner method".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
            },
        );

        proto::jobworkerp::data::RunnerData {
            name: name.to_string(),
            description: "test runner desc".to_string(),
            runner_settings_proto: include_str!("../../../proto/protobuf/test_runner.proto")
                .to_string(),
            runner_type: RunnerType::Plugin as i32,
            definition: "./target/debug/libplugin_runner_test.so".to_string(),
            method_proto_map: Some(proto::jobworkerp::data::MethodProtoMap { schemas }),
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

        // Phase 6.6.7: Use new method_descriptors HashMap
        let mut method_descriptors = std::collections::HashMap::new();

        if let Some(method_proto_map) = &runner_data.method_proto_map {
            for (method_name, method_schema) in &method_proto_map.schemas {
                let args_descriptor = if !method_schema.args_proto.is_empty() {
                    command_utils::protobuf::ProtobufDescriptor::new(&method_schema.args_proto).ok()
                } else {
                    None
                };

                let result_descriptor = if !method_schema.result_proto.is_empty() {
                    command_utils::protobuf::ProtobufDescriptor::new(&method_schema.result_proto)
                        .ok()
                } else {
                    None
                };

                method_descriptors.insert(
                    method_name.clone(),
                    super::MethodDescriptors {
                        args_descriptor,
                        result_descriptor,
                    },
                );
            }
        }

        // Legacy fields: populate with "run" method descriptors for backward compatibility
        let args_descriptor = method_descriptors
            .get("run")
            .and_then(|desc| desc.args_descriptor.clone());
        let result_descriptor = method_descriptors
            .get("run")
            .and_then(|desc| desc.result_descriptor.clone());

        RunnerDataWithDescriptor {
            runner_data: runner_data.clone(),
            runner_settings_descriptor: Some(
                command_utils::protobuf::ProtobufDescriptor::new(
                    &runner_data.runner_settings_proto,
                )
                .unwrap(),
            ),
            method_descriptors,
            args_descriptor,
            result_descriptor,
        }
    }

    // Phase 6.6.7.7: Helper functions for multi-method runner testing
    pub fn test_multi_method_runner_data(name: &str) -> RunnerData {
        // MCP Server with 5 methods
        let mut schemas = std::collections::HashMap::new();

        // Method 1: run (default method)
        schemas.insert(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../../proto/protobuf/test_args.proto").to_string(),
                result_proto: include_str!("../../../proto/protobuf/test_result.proto").to_string(),
                description: Some("Default run method".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
            },
        );

        // Method 2: list_files
        schemas.insert(
            "list_files".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: r#"
                    syntax = "proto3";
                    package test;
                    message ListFilesArgs {
                        string directory = 1;
                    }
                "#
                .to_string(),
                result_proto: r#"
                    syntax = "proto3";
                    package test;
                    message ListFilesResult {
                        repeated string files = 1;
                    }
                "#
                .to_string(),
                description: Some("List files in directory".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
            },
        );

        // Method 3: read_file
        schemas.insert(
            "read_file".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: r#"
                    syntax = "proto3";
                    package test;
                    message ReadFileArgs {
                        string path = 1;
                    }
                "#
                .to_string(),
                result_proto: r#"
                    syntax = "proto3";
                    package test;
                    message ReadFileResult {
                        string content = 1;
                    }
                "#
                .to_string(),
                description: Some("Read file content".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
            },
        );

        // Method 4: write_file (empty result_proto case)
        schemas.insert(
            "write_file".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: r#"
                    syntax = "proto3";
                    package test;
                    message WriteFileArgs {
                        string path = 1;
                        string content = 2;
                    }
                "#
                .to_string(),
                result_proto: String::new(), // Empty result_proto
                description: Some("Write file content".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
            },
        );

        // Method 5: delete_file (another empty result_proto case)
        schemas.insert(
            "delete_file".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: r#"
                    syntax = "proto3";
                    package test;
                    message DeleteFileArgs {
                        string path = 1;
                    }
                "#
                .to_string(),
                result_proto: String::new(), // Empty result_proto
                description: Some("Delete file".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
            },
        );

        proto::jobworkerp::data::RunnerData {
            name: name.to_string(),
            description: "MCP Server with 5 methods".to_string(),
            runner_settings_proto: include_str!("../../../proto/protobuf/test_runner.proto")
                .to_string(),
            runner_type: RunnerType::Plugin as i32,
            definition: "./target/debug/libmcp_server_test.so".to_string(),
            method_proto_map: Some(proto::jobworkerp::data::MethodProtoMap { schemas }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::runner::test::*;

    #[test]
    fn test_parse_proto_schemas_single_method_runner() {
        // Test single-method runner (only "run" method)
        let runner_data = test_runner_data("single_method_runner");

        let result = RunnerDataWithDescriptor::parse_proto_schemas_from_runner_data(runner_data);
        assert!(result.is_ok(), "parse_proto_schemas should succeed");

        let parsed = result.unwrap();

        // Verify method_descriptors has exactly 1 entry
        assert_eq!(
            parsed.method_descriptors.len(),
            1,
            "Should have exactly 1 method descriptor"
        );

        // Verify "run" method exists
        assert!(
            parsed.method_descriptors.contains_key("run"),
            "Should have 'run' method"
        );

        // Verify "run" method has args_descriptor (not empty proto)
        let run_desc = parsed.method_descriptors.get("run").unwrap();
        assert!(
            run_desc.args_descriptor.is_some(),
            "args_descriptor should be Some for non-empty proto"
        );

        // Verify "run" method has no result_descriptor (empty proto)
        assert!(
            run_desc.result_descriptor.is_none(),
            "result_descriptor should be None for empty result_proto"
        );

        // Verify legacy fields are populated from "run" method
        assert!(
            parsed.args_descriptor.is_some(),
            "Legacy args_descriptor should be populated"
        );
        assert!(
            parsed.result_descriptor.is_none(),
            "Legacy result_descriptor should be None"
        );
    }

    #[test]
    fn test_parse_proto_schemas_multi_method_runner() {
        // Test multi-method runner (MCP Server with 5 methods)
        let runner_data = test_multi_method_runner_data("multi_method_runner");

        let result = RunnerDataWithDescriptor::parse_proto_schemas_from_runner_data(runner_data);
        assert!(result.is_ok(), "parse_proto_schemas should succeed");

        let parsed = result.unwrap();

        // Verify method_descriptors has exactly 5 entries
        assert_eq!(
            parsed.method_descriptors.len(),
            5,
            "Should have exactly 5 method descriptors"
        );

        // Verify all 5 methods exist
        assert!(
            parsed.method_descriptors.contains_key("run"),
            "Should have 'run' method"
        );
        assert!(
            parsed.method_descriptors.contains_key("list_files"),
            "Should have 'list_files' method"
        );
        assert!(
            parsed.method_descriptors.contains_key("read_file"),
            "Should have 'read_file' method"
        );
        assert!(
            parsed.method_descriptors.contains_key("write_file"),
            "Should have 'write_file' method"
        );
        assert!(
            parsed.method_descriptors.contains_key("delete_file"),
            "Should have 'delete_file' method"
        );

        // Verify each method has correct descriptors
        let run_desc = parsed.method_descriptors.get("run").unwrap();
        assert!(
            run_desc.args_descriptor.is_some(),
            "run: args_descriptor should be Some"
        );
        assert!(
            run_desc.result_descriptor.is_some(),
            "run: result_descriptor should be Some"
        );

        let list_files_desc = parsed.method_descriptors.get("list_files").unwrap();
        assert!(
            list_files_desc.args_descriptor.is_some(),
            "list_files: args_descriptor should be Some"
        );
        assert!(
            list_files_desc.result_descriptor.is_some(),
            "list_files: result_descriptor should be Some"
        );

        let read_file_desc = parsed.method_descriptors.get("read_file").unwrap();
        assert!(
            read_file_desc.args_descriptor.is_some(),
            "read_file: args_descriptor should be Some"
        );
        assert!(
            read_file_desc.result_descriptor.is_some(),
            "read_file: result_descriptor should be Some"
        );

        // Test empty result_proto cases
        let write_file_desc = parsed.method_descriptors.get("write_file").unwrap();
        assert!(
            write_file_desc.args_descriptor.is_some(),
            "write_file: args_descriptor should be Some"
        );
        assert!(
            write_file_desc.result_descriptor.is_none(),
            "write_file: result_descriptor should be None for empty result_proto"
        );

        let delete_file_desc = parsed.method_descriptors.get("delete_file").unwrap();
        assert!(
            delete_file_desc.args_descriptor.is_some(),
            "delete_file: args_descriptor should be Some"
        );
        assert!(
            delete_file_desc.result_descriptor.is_none(),
            "delete_file: result_descriptor should be None for empty result_proto"
        );

        // Verify legacy fields are populated from "run" method
        assert!(
            parsed.args_descriptor.is_some(),
            "Legacy args_descriptor should be populated from 'run' method"
        );
        assert!(
            parsed.result_descriptor.is_some(),
            "Legacy result_descriptor should be populated from 'run' method"
        );
    }

    #[test]
    fn test_get_args_descriptor_for_method() {
        let runner_data = test_multi_method_runner_data("test_runner");

        let parsed =
            RunnerDataWithDescriptor::parse_proto_schemas_from_runner_data(runner_data).unwrap();

        // Test default method (using = None)
        let default_desc = parsed.get_args_descriptor_for_method(None);
        assert!(
            default_desc.is_some(),
            "Default method should return descriptor"
        );

        // Test explicit "run" method
        let run_desc = parsed.get_args_descriptor_for_method(Some("run"));
        assert!(run_desc.is_some(), "'run' method should return descriptor");

        // Test other methods
        let list_files_desc = parsed.get_args_descriptor_for_method(Some("list_files"));
        assert!(
            list_files_desc.is_some(),
            "'list_files' method should return descriptor"
        );

        let read_file_desc = parsed.get_args_descriptor_for_method(Some("read_file"));
        assert!(
            read_file_desc.is_some(),
            "'read_file' method should return descriptor"
        );

        let write_file_desc = parsed.get_args_descriptor_for_method(Some("write_file"));
        assert!(
            write_file_desc.is_some(),
            "'write_file' method should return descriptor"
        );

        // Test non-existent method
        let non_existent_desc = parsed.get_args_descriptor_for_method(Some("non_existent"));
        assert!(
            non_existent_desc.is_none(),
            "Non-existent method should return None"
        );
    }

    #[test]
    fn test_get_result_descriptor_for_method() {
        let runner_data = test_multi_method_runner_data("test_runner");

        let parsed =
            RunnerDataWithDescriptor::parse_proto_schemas_from_runner_data(runner_data).unwrap();

        // Test default method (using = None)
        let default_desc = parsed.get_result_descriptor_for_method(None);
        assert!(
            default_desc.is_some(),
            "Default method should return descriptor"
        );

        // Test methods with result_proto
        let run_desc = parsed.get_result_descriptor_for_method(Some("run"));
        assert!(run_desc.is_some(), "'run' method should return descriptor");

        let list_files_desc = parsed.get_result_descriptor_for_method(Some("list_files"));
        assert!(
            list_files_desc.is_some(),
            "'list_files' method should return descriptor"
        );

        // Test methods with empty result_proto
        let write_file_desc = parsed.get_result_descriptor_for_method(Some("write_file"));
        assert!(
            write_file_desc.is_none(),
            "'write_file' method should return None for empty result_proto"
        );

        let delete_file_desc = parsed.get_result_descriptor_for_method(Some("delete_file"));
        assert!(
            delete_file_desc.is_none(),
            "'delete_file' method should return None for empty result_proto"
        );

        // Test non-existent method
        let non_existent_desc = parsed.get_result_descriptor_for_method(Some("non_existent"));
        assert!(
            non_existent_desc.is_none(),
            "Non-existent method should return None"
        );
    }

    #[test]
    fn test_get_job_args_message_for_method() {
        let runner_data = test_multi_method_runner_data("test_runner");

        let parsed =
            RunnerDataWithDescriptor::parse_proto_schemas_from_runner_data(runner_data).unwrap();

        // Test default method (using = None)
        let default_msg = parsed.get_job_args_message_for_method(None);
        assert!(
            default_msg.is_ok(),
            "Default method should return Ok result"
        );

        // Test explicit methods
        let run_msg = parsed.get_job_args_message_for_method(Some("run"));
        assert!(run_msg.is_ok(), "'run' method should return Ok result");

        let list_files_msg = parsed.get_job_args_message_for_method(Some("list_files"));
        assert!(
            list_files_msg.is_ok(),
            "'list_files' method should return Ok result"
        );

        // Test non-existent method (returns Ok(None), not an error)
        let non_existent_msg = parsed.get_job_args_message_for_method(Some("non_existent"));
        assert!(
            non_existent_msg.is_ok(),
            "Non-existent method should return Ok(None)"
        );
        assert!(
            non_existent_msg.unwrap().is_none(),
            "Non-existent method should have None descriptor"
        );
    }

    #[test]
    fn test_get_job_result_message_for_method() {
        let runner_data = test_multi_method_runner_data("test_runner");

        let parsed =
            RunnerDataWithDescriptor::parse_proto_schemas_from_runner_data(runner_data).unwrap();

        // Test methods with result_proto
        let run_msg = parsed.get_job_result_message_for_method(Some("run"));
        assert!(run_msg.is_ok(), "'run' method should return Ok result");

        let list_files_msg = parsed.get_job_result_message_for_method(Some("list_files"));
        assert!(
            list_files_msg.is_ok(),
            "'list_files' method should return Ok result"
        );

        // Test methods with empty result_proto (should return Ok(None))
        let write_file_msg = parsed.get_job_result_message_for_method(Some("write_file"));
        assert!(
            write_file_msg.is_ok(),
            "'write_file' method should return Ok(None)"
        );
        assert!(
            write_file_msg.unwrap().is_none(),
            "'write_file' should have None descriptor"
        );

        // Test non-existent method (returns Ok(None), not an error)
        let non_existent_msg = parsed.get_job_result_message_for_method(Some("non_existent"));
        assert!(
            non_existent_msg.is_ok(),
            "Non-existent method should return Ok(None)"
        );
        assert!(
            non_existent_msg.unwrap().is_none(),
            "Non-existent method should have None descriptor"
        );
    }
}
