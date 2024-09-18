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
use proto::jobworkerp::data::{RunnerSchema, RunnerSchemaData, RunnerSchemaId};
use std::{fmt, sync::Arc, time::Duration};

#[async_trait]
pub trait RunnerSchemaApp: fmt::Debug + Send + Sync {
    async fn create_runner_schema(&self, runner_schema: RunnerSchemaData)
        -> Result<RunnerSchemaId>;

    // offset, limit 付きのリストキャッシュは更新されないので反映に時間かかる (ttl短かめにする)
    async fn update_runner_schema(
        &self,
        id: &RunnerSchemaId,
        runner_schema: &Option<RunnerSchemaData>,
    ) -> Result<bool>;
    async fn delete_runner_schema(&self, id: &RunnerSchemaId) -> Result<bool>;

    async fn find_runner_schema(
        &self,
        id: &RunnerSchemaId,
        ttl: Option<&Duration>,
    ) -> Result<Option<RunnerSchema>>
    where
        Self: Send + 'static;

    async fn find_runner_schema_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
        ttl: Option<&Duration>,
    ) -> Result<Vec<RunnerSchema>>
    where
        Self: Send + 'static;

    async fn find_runner_schema_all_list(
        &self,
        ttl: Option<&Duration>,
    ) -> Result<Vec<RunnerSchema>>
    where
        Self: Send + 'static;

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static;
}

pub trait UseRunnerSchemaParserWithCache: Send + Sync {
    fn cache(&self) -> &MemoryCacheImpl<Arc<String>, RunnerSchemaWithDescriptor>;

    fn default_ttl(&self) -> Option<&Duration> {
        None
    }
    fn _cache_key(id: &RunnerSchemaId) -> Arc<String> {
        Arc::new(format!("schema:{}", id.value))
    }

    fn clear_cache_with_descriptor(
        &self,
        schema_id: &RunnerSchemaId,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async {
            let key = Self::_cache_key(schema_id);
            self.cache().delete_cache_locked(&key).await
        }
    }

    fn clear_cache_th_descriptor(&self) -> impl std::future::Future<Output = Result<()>> + Send {
        async { self.cache().clear().await }
    }
    fn validate_and_get_runner_schema(
        &self,
        schema: RunnerSchemaData,
    ) -> Result<RunnerSchemaWithDescriptor> {
        // operation
        let ope_d = ProtobufDescriptor::new(&schema.operation_proto).map_err(|e| {
            JobWorkerError::ParseError(format!("runner schema operation_proto error:{:?})", e))
        })?;
        let ope_m = ope_d
            .get_message_by_name(&RunnerSchemaWithDescriptor::operation_message_name(
                &schema.name,
            ))
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "illegal RunnerSchemaData: message name is not found:{} from {}",
                RunnerSchemaWithDescriptor::operation_message_name(schema.name.as_str()),
                schema.operation_proto
            )))?;

        // job_arg_proto
        let arg_d = ProtobufDescriptor::new(&schema.job_arg_proto).map_err(|e| {
            JobWorkerError::ParseError(format!("runner schema job_arg_proto error:{:?})", e))
        })?;
        let arg_m = arg_d
            .get_message_by_name(&RunnerSchemaWithDescriptor::job_arg_message_name(
                &schema.name,
            ))
            .ok_or(JobWorkerError::InvalidParameter(format!(
                "illegal RunnerSchemaData: message name is not found:{} from {}",
                RunnerSchemaWithDescriptor::job_arg_message_name(schema.name.as_str()),
                schema.job_arg_proto
            )))?;
        if ope_m.package_name().starts_with("jobworkerp.") {
            return Err(JobWorkerError::InvalidParameter(format!(
                "illegal RunnerSchemaData: operation_proto package is invalid: cannot use system package name (jobworkerp): {}",
                schema.operation_proto
            )).into());
        }
        if arg_m.package_name().starts_with("jobworkerp.") {
            return Err(JobWorkerError::InvalidParameter(format!(
                "illegal RunnerSchemaData: job_arg_proto package is invalid: cannot use system package name (jobworkerp): {}",
                schema.job_arg_proto
            )).into());
        }
        Ok(RunnerSchemaWithDescriptor {
            schema,
            operation_descriptor: ope_d,
            args_descriptor: arg_d,
        })
    }

    fn store_proto_cache(
        &self,
        schema_id: &RunnerSchemaId,
        schema_with_descriptor: &RunnerSchemaWithDescriptor,
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
        schema_id: &RunnerSchemaId,
        schema: &RunnerSchemaData,
    ) -> impl std::future::Future<Output = Result<RunnerSchemaWithDescriptor>> + Send {
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
                    Ok(RunnerSchemaWithDescriptor {
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
pub struct RunnerSchemaWithDescriptor {
    pub schema: RunnerSchemaData,
    pub operation_descriptor: ProtobufDescriptor,
    pub args_descriptor: ProtobufDescriptor,
}
impl RunnerSchemaWithDescriptor {
    fn operation_message_name(name: &str) -> String {
        format!("{}Operation", name)
    }

    fn job_arg_message_name(name: &str) -> String {
        format!("{}Arg", name)
    }
    #[allow(dead_code)]
    fn get_operation_message(&self) -> Result<MessageDescriptor> {
        self.operation_descriptor
            .get_message_by_name(&Self::job_arg_message_name(&self.schema.name))
            .ok_or(
                JobWorkerError::InvalidParameter(format!(
                    "illegal RunnerSchemaData: message name is not found:{} from {}",
                    RunnerSchemaWithDescriptor::operation_message_name(self.schema.name.as_str()),
                    &self.schema.operation_proto
                ))
                .into(),
            )
    }
    #[allow(dead_code)]
    fn get_job_arg_message(&self) -> Result<MessageDescriptor> {
        self.args_descriptor
            .get_message_by_name(&Self::job_arg_message_name(&self.schema.name))
            .ok_or(
                JobWorkerError::InvalidParameter(format!(
                    "illegal RunnerSchemaData: message name is not found:{} from {}",
                    RunnerSchemaWithDescriptor::job_arg_message_name(self.schema.name.as_str()),
                    &self.schema.job_arg_proto
                ))
                .into(),
            )
    }
}

pub trait RunnerSchemaCacheHelper {
    fn find_cache_key(id: &i64) -> Arc<String> {
        Arc::new(["runner_schema_id:", &id.to_string()].join(""))
    }

    // lifetime issue
    // fn find_list_cache_key(limit: Option<&i32>, offset: Option<&i64>) -> String {
    //     if let Some(l) = limit {
    //         [
    //             "runner_schema_list:",
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
        Arc::new("runner_schema_list:all".to_string())
    }
}
pub trait UseRunnerSchemaApp {
    fn runner_schema_app(&self) -> &Arc<dyn RunnerSchemaApp + 'static>;
}
