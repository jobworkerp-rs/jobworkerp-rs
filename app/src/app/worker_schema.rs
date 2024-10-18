pub mod hybrid;
pub mod rdb;
pub mod redis;

use anyhow::Result;
use async_trait::async_trait;
use infra::error::JobWorkerError;
use infra_utils::infra::{
    memory::{MemoryCacheImpl, UseMemoryCache},
    protobuf::ProtobufDescriptor,
};
use prost_reflect::MessageDescriptor;
use proto::jobworkerp::data::{WorkerSchema, WorkerSchemaData, WorkerSchemaId};
use std::{fmt, sync::Arc, time::Duration};

#[async_trait]
pub trait WorkerSchemaApp: fmt::Debug + Send + Sync {
    // async fn create_worker_schema(&self, worker_schema: WorkerSchemaData)
    //     -> Result<WorkerSchemaId>;
    // TODO update by reloading plugin files
    // async fn update_worker_schema(
    //     &self,
    //     id: &WorkerSchemaId,
    //     worker_schema: &Option<WorkerSchemaData>,
    // ) -> Result<bool>;

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
}

pub trait UseWorkerSchemaParserWithCache: Send + Sync {
    fn cache(&self) -> &MemoryCacheImpl<Arc<String>, WorkerSchemaWithDescriptor>;

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
            self.cache().delete_cache_locked(&key).await
        }
    }

    fn clear_cache_th_descriptor(&self) -> impl std::future::Future<Output = Result<()>> + Send {
        async { self.cache().clear().await }
    }
    // TODO remove if not used
    fn validate_and_get_worker_schema(
        &self,
        schema: WorkerSchemaData,
    ) -> Result<WorkerSchemaWithDescriptor> {
        // operation_proto
        let ope_d = ProtobufDescriptor::new(&schema.operation_proto).map_err(|e| {
            JobWorkerError::ParseError(format!("runner schema operation_proto error:{:?})", e))
        })?;
        // job_arg_proto
        let arg_d = ProtobufDescriptor::new(&schema.job_arg_proto).map_err(|e| {
            JobWorkerError::ParseError(format!("runner schema job_arg_proto error:{:?})", e))
        })?;
        let ope_m = ope_d
            .get_message_by_name(&WorkerSchemaWithDescriptor::operation_message_name(
                &schema.name,
            ))
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "illegal WorkerSchemaData: message name is not found:{} from {}",
                WorkerSchemaWithDescriptor::operation_message_name(schema.name.as_str()),
                schema.job_arg_proto
            )))?;
        let arg_m = arg_d
            .get_message_by_name(&WorkerSchemaWithDescriptor::job_args_message_name(
                &schema.name,
            ))
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "illegal WorkerSchemaData: message name is not found:{} from {}",
                WorkerSchemaWithDescriptor::job_args_message_name(schema.name.as_str()),
                schema.job_arg_proto
            )))?;
        if ope_m.package_name().starts_with("jobworkerp.") {
            return Err(JobWorkerError::InvalidParameter(format!(
                "illegal WorkerSchemaData: operation_proto package is invalid: cannot use system package name (jobworkerp): {}",
                schema.job_arg_proto
            )).into());
        }
        if arg_m.package_name().starts_with("jobworkerp.") {
            return Err(JobWorkerError::InvalidParameter(format!(
                "illegal WorkerSchemaData: job_arg_proto package is invalid: cannot use system package name (jobworkerp): {}",
                schema.job_arg_proto
            )).into());
        }
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
            self.cache()
                .set_and_wait_cache_locked(key, schema_with_descriptor.clone(), self.default_ttl())
                .await
        }
    }
    fn parse_proto_with_cache(
        &self,
        schema_id: &WorkerSchemaId,
        schema: &WorkerSchemaData,
    ) -> impl std::future::Future<Output = Result<WorkerSchemaWithDescriptor>> + Send {
        async {
            let key = Self::_cache_key(schema_id);
            self.cache()
                .with_cache_locked(&key, self.default_ttl(), || async {
                    let ope_d = ProtobufDescriptor::new(&schema.operation_proto).map_err(|e| {
                        JobWorkerError::ParseError(format!(
                            "runner schema operation_proto error:{:?})",
                            e
                        ))
                    })?;
                    let arg_d = ProtobufDescriptor::new(&schema.job_arg_proto).map_err(|e| {
                        JobWorkerError::ParseError(format!(
                            "runner schema job_arg_proto error:{:?})",
                            e
                        ))
                    })?;
                    Ok(WorkerSchemaWithDescriptor {
                        schema: schema.clone(),
                        operation_descriptor: ope_d,
                        args_descriptor: arg_d,
                    })
                })
                .await
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkerSchemaWithDescriptor {
    pub schema: WorkerSchemaData,
    pub operation_descriptor: ProtobufDescriptor,
    pub args_descriptor: ProtobufDescriptor,
}
impl WorkerSchemaWithDescriptor {
    fn operation_message_name(name: &str) -> String {
        format!("{}Operation", name)
    }
    #[allow(dead_code)]
    fn get_operation_message(&self) -> Result<MessageDescriptor> {
        self.operation_descriptor
            .get_message_by_name(&Self::operation_message_name(&self.schema.name))
            .ok_or(
                JobWorkerError::InvalidParameter(format!(
                    "illegal WorkerSchemaData: operation message name is not found:{} from:\n {}",
                    WorkerSchemaWithDescriptor::operation_message_name(self.schema.name.as_str()),
                    &self.schema.operation_proto
                ))
                .into(),
            )
    }
    fn job_args_message_name(name: &str) -> String {
        format!("{}Arg", name)
    }
    #[allow(dead_code)]
    fn get_job_arg_message(&self) -> Result<MessageDescriptor> {
        self.args_descriptor
            .get_message_by_name(&Self::job_args_message_name(&self.schema.name))
            .ok_or(
                JobWorkerError::InvalidParameter(format!(
                    "illegal WorkerSchemaData: job_args message name is not found:{} from {}",
                    WorkerSchemaWithDescriptor::job_args_message_name(self.schema.name.as_str()),
                    &self.schema.job_arg_proto
                ))
                .into(),
            )
    }
}

pub trait WorkerSchemaCacheHelper {
    fn find_cache_key(id: &i64) -> Arc<String> {
        Arc::new(["worker_schema_id:", &id.to_string()].join(""))
    }

    // lifetime issue
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
pub trait UseWorkerSchemaApp {
    fn worker_schema_app(&self) -> &Arc<dyn WorkerSchemaApp + 'static>;
}
