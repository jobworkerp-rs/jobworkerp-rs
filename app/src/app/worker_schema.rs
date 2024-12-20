pub mod hybrid;
pub mod rdb;
pub mod redis;

use anyhow::Result;
use async_trait::async_trait;
use command_utils::protobuf::ProtobufDescriptor;
use command_utils::util::result::FlatMap;
use infra::error::JobWorkerError;
use infra_utils::infra::memory::{MemoryCacheImpl, UseMemoryCache};
use prost_reflect::{DynamicMessage, MessageDescriptor};
use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaData, WorkerSchemaId};
use std::{fmt, future::Future, sync::Arc, time::Duration};

#[async_trait]
pub trait WorkerSchemaApp: fmt::Debug + Send + Sync {
    // load new schema from plugin files and store it
    async fn load_worker_schema(&self) -> Result<bool>;

    async fn delete_worker_schema(&self, id: &WorkerSchemaId) -> Result<bool>;

    async fn find_worker_schema(
        &self,
        id: &WorkerSchemaId,
        ttl: Option<&Duration>,
    ) -> Result<Option<WorkerSchema>>
    where
        Self: Send + 'static;

    async fn find_worker_schema_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        ttl: Option<&Duration>,
    ) -> Result<Vec<WorkerSchema>>
    where
        Self: Send + 'static;

    async fn find_worker_schema_all_list(
        &self,
        ttl: Option<&Duration>,
    ) -> Result<Vec<WorkerSchema>>
    where
        Self: Send + 'static;

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static;

    // for test
    #[cfg(test)]
    async fn create_test_schema(
        &self,
        schema_id: &WorkerSchemaId,
        name: &str,
    ) -> Result<WorkerSchemaWithDescriptor>;
}

pub trait UseWorkerSchemaApp: Send + Sync {
    fn worker_schema_app(&self) -> Arc<dyn WorkerSchemaApp>;
}

pub trait UseWorkerSchemaParserWithCache: Send + Sync {
    fn descriptor_cache(&self) -> &MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor>;

    fn default_ttl(&self) -> Option<&Duration> {
        None
    }
    fn _cache_key(id: &WorkerSchemaId) -> Arc<String> {
        Arc::new(format!("schema:{}", id.value))
    }

    fn clear_cache_with_descriptor(
        &self,
        schema_id: &WorkerSchemaId,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async {
            let key = Self::_cache_key(schema_id);
            self.descriptor_cache().delete_cache_locked(&key).await
        }
    }

    fn clear_cache_th_descriptor(&self) -> impl std::future::Future<Output = Result<()>> + Send {
        async { self.descriptor_cache().clear().await }
    }
    // TODO remove if not used
    fn parse_worker_schema(&self, schema: WorkerSchemaData) -> Result<WorkerSchemaWithDescriptor> {
        // operation_proto
        let ope_d = if schema.operation_proto.is_empty() {
            None
        } else {
            let ope_d = ProtobufDescriptor::new(&schema.operation_proto).map_err(|e| {
                JobWorkerError::ParseError(format!("runner schema operation_proto error:{:?}", e))
            })?;
            let _ope_m = ope_d
                .get_messages()
                .first()
                .ok_or(JobWorkerError::InvalidParameter(format!(
                    "illegal WorkerSchemaData: message name is not found from {}",
                    schema.operation_proto
                )))?;
            Some(ope_d)
        };
        // job_arg_proto
        let arg_d = if schema.job_arg_proto.is_empty() {
            // use JobResult as job_arg_proto
            None
        } else {
            let arg_d = ProtobufDescriptor::new(&schema.job_arg_proto).map_err(|e| {
                JobWorkerError::ParseError(format!("runner schema job_arg_proto error:{:?}", e))
            })?;
            let _arg_m = arg_d
                .get_messages()
                .first()
                .ok_or(JobWorkerError::InvalidParameter(format!(
                    "illegal WorkerSchemaData: message name is not found from {}",
                    schema.job_arg_proto
                )))?;
            Some(arg_d)
        };
        Ok(WorkerSchemaWithDescriptor {
            schema,
            operation_descriptor: ope_d,
            args_descriptor: arg_d,
        })
    }

    fn store_proto_cache(
        &self,
        schema_id: &WorkerSchemaId,
        schema_with_descriptor: &WorkerSchemaWithDescriptor,
    ) -> impl std::future::Future<Output = bool> + Send {
        async {
            let key = Self::_cache_key(schema_id);
            self.descriptor_cache()
                .set_and_wait_cache_locked(key, schema_with_descriptor.clone(), self.default_ttl())
                .await
        }
    }
    fn parse_proto_with_cache(
        &self,
        schema_id: &WorkerSchemaId,
        schema: &WorkerSchemaData,
    ) -> impl Future<Output = Result<WorkerSchemaWithDescriptor>> + Send {
        async {
            let key = Self::_cache_key(schema_id);
            self.descriptor_cache()
                .with_cache_locked(&key, self.default_ttl(), || async {
                    self.parse_worker_schema(schema.clone())
                })
                .await
        }
    }
    fn validate_operation_data_with_schema(
        &self,
        schema_id: &WorkerSchemaId,
        schema: &WorkerSchemaData,
        operation: &[u8],
    ) -> impl Future<Output = Result<Option<DynamicMessage>>> + Send {
        async move {
            let schema_with_descriptor = self.parse_proto_with_cache(schema_id, schema).await?;
            schema_with_descriptor.parse_operation_data(operation)
        }
    }
}
pub trait UseWorkerSchemaAppParserWithCache:
    UseWorkerSchemaApp + UseWorkerSchemaParserWithCache + Send + Sync
{
    fn validate_operation_data(
        &self,
        schema_id: &WorkerSchemaId,
        operation: &[u8],
    ) -> impl Future<Output = Result<Option<DynamicMessage>>> + Send {
        let worker_schema_app = self.worker_schema_app().clone();
        async move {
            if let Some(WorkerSchema {
                id: _,
                data: Some(schema),
            }) = {
                worker_schema_app
                    .find_worker_schema(schema_id, self.default_ttl())
                    .await?
            } {
                self.validate_operation_data_with_schema(schema_id, &schema, operation)
                    .await
            } else {
                Err(JobWorkerError::InvalidParameter(format!(
                    "illegal WorkerSchemaData: schema is not found:{}",
                    schema_id.value
                ))
                .into())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkerSchemaWithDescriptor {
    pub schema: WorkerSchemaData,
    pub operation_descriptor: Option<ProtobufDescriptor>,
    pub args_descriptor: Option<ProtobufDescriptor>,
}
impl WorkerSchemaWithDescriptor {
    pub fn get_operation_message(&self) -> Result<Option<MessageDescriptor>> {
        if let Some(op) = &self.operation_descriptor {
            op.get_messages()
                .first()
                .cloned()
                .ok_or(
                    JobWorkerError::InvalidParameter(format!(
                        "illegal WorkerSchemaData: operation message is not found from:\n {}",
                        &self.schema.operation_proto
                    ))
                    .into(),
                )
                .map(Some)
        } else {
            Ok(None)
        }
    }
    pub fn parse_operation_data(&self, operation: &[u8]) -> Result<Option<DynamicMessage>> {
        if let Some(op) = &self.operation_descriptor {
            self.get_operation_message()?
                .ok_or(
                    JobWorkerError::InvalidParameter(format!(
                        "illegal WorkerSchemaData: operation message is not found from:\n {}",
                        &self.schema.operation_proto
                    ))
                    .into(),
                )
                .flat_map(|m| {
                    op.get_message_by_name_from_bytes(m.full_name(), operation)
                        .map(Some)
                        .map_err(|e| {
                            JobWorkerError::InvalidParameter(format!(
                                "illegal operation data: cannot parse operation data as {}: {:?}",
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
    pub fn get_job_arg_message(&self) -> Result<Option<MessageDescriptor>> {
        if let Some(op) = &self.args_descriptor {
            op.get_messages()
                .first()
                .cloned()
                .ok_or(
                    JobWorkerError::InvalidParameter(format!(
                        "illegal WorkerSchemaData: job args message name is not found from:\n {}",
                        &self.schema.job_arg_proto
                    ))
                    .into(),
                )
                .map(Some)
        } else {
            Ok(None)
        }
    }
}

pub trait WorkerSchemaCacheHelper {
    fn find_cache_key(id: &i64) -> Arc<String> {
        Arc::new(["worker_schema_id:", &id.to_string()].join(""))
    }

    // XXX lifetime issue
    // fn find_list_cache_key(limit: Option<&i32>, offset: Option<&i64>) -> String {
    //     if let Some(l) = limit {
    //         [
    //             "worker_schema_list:",
    //             l.to_string().as_str(),
    //             ":",
    //             offset.unwrap_or(&0i64).to_string().as_str(),
    //         ]
    //         .join("")
    //     } else {
    //         Self::find_all_list_cache_key()
    //     }
    // }
    fn find_all_list_cache_key() -> Arc<String> {
        Arc::new("worker_schema_list:all".to_string())
    }
}

#[cfg(test)]
pub mod test {
    use super::WorkerSchemaWithDescriptor;
    use proto::jobworkerp::data::{RunnerType, WorkerSchemaData};
    pub fn test_worker_schema(name: &str) -> WorkerSchemaData {
        proto::jobworkerp::data::WorkerSchemaData {
            name: name.to_string(),
            operation_proto: include_str!("../../../proto/protobuf/test_operation.proto")
                .to_string(),
            job_arg_proto: include_str!("../../../proto/protobuf/test_args.proto").to_string(),
            runner_type: RunnerType::Plugin as i32,
            result_output_proto: None,
        }
    }

    pub fn test_worker_schema_with_descriptor(name: &str) -> WorkerSchemaWithDescriptor {
        let schema = test_worker_schema(name);
        WorkerSchemaWithDescriptor {
            schema: schema.clone(),
            operation_descriptor: Some(
                command_utils::protobuf::ProtobufDescriptor::new(&schema.operation_proto).unwrap(),
            ),
            args_descriptor: Some(
                command_utils::protobuf::ProtobufDescriptor::new(&schema.job_arg_proto).unwrap(),
            ),
        }
    }
}
