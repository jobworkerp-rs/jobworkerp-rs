pub mod event;
pub mod function_set;
pub mod job;
pub mod job_result;
pub mod module;
pub mod resource;
pub mod runner;
pub mod worker;

use anyhow::Result;
use command_utils::util::{
    datetime,
    id_generator::{self, IDGenerator, MockIdGenerator},
};
use debug_stub_derive::DebugStub;
use infra_utils::infra::{rdb::RdbConfig, redis::RedisConfig};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{Job, JobData};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

use self::resource::{load_db_config_from_env, load_redis_config_from_env};

#[derive(Clone, DebugStub)]
pub struct IdGeneratorWrapper {
    #[debug_stub = "IDGenerator"]
    id_generator: Arc<Mutex<IDGenerator>>,
}

impl IdGeneratorWrapper {
    pub fn new() -> Self {
        IdGeneratorWrapper {
            id_generator: Arc::new(Mutex::new(id_generator::new_generator_by_ip())),
        }
    }
    // for test
    pub fn new_mock() -> Self {
        IdGeneratorWrapper {
            id_generator: Arc::new(Mutex::new(IDGenerator::Mock(MockIdGenerator::new()))),
        }
    }
    // thread safe
    pub fn generate_id(&self) -> Result<i64> {
        self.id_generator
            .lock()
            .map_err(|e| JobWorkerError::GenerateIdError(e.to_string()).into())
            .and_then(|mut g| g.generate())
    }
    pub fn get_id_generator(&mut self) -> Arc<Mutex<IDGenerator>> {
        self.id_generator.clone()
    }
}

impl Default for IdGeneratorWrapper {
    fn default() -> Self {
        Self::new()
    }
}

pub trait UseIdGenerator {
    fn id_generator(&self) -> &IdGeneratorWrapper;
}

#[derive(Deserialize, Clone, Debug)]
pub struct JobQueueConfig {
    /// seconds for expire direct or run after job_result queue
    pub expire_job_result_seconds: u32,
    /// msec for periodic or run_after job
    pub fetch_interval: u32,
}

impl Default for JobQueueConfig {
    fn default() -> Self {
        tracing::info!("Use default JobQueueConfig.");
        Self {
            expire_job_result_seconds: 24 * 60 * 60, // 1day
            fetch_interval: 1000,                    // 5sec
        }
    }
}

pub fn load_job_queue_config_from_env() -> Result<JobQueueConfig> {
    envy::prefixed("JOB_QUEUE_")
        .from_env::<JobQueueConfig>()
        .map_err(|e| {
            JobWorkerError::RuntimeError(format!("cannot read redis config from env: {:?}", e))
                .into()
        })
}

pub trait UseJobQueueConfig {
    fn job_queue_config(&self) -> &JobQueueConfig;

    // determine not enqueue immediately
    fn is_run_after_job_data(&self, data: &JobData) -> bool {
        data.run_after_time > datetime::now_millis() + self.job_queue_config().fetch_interval as i64
    }
    fn is_run_after_job(&self, job: &Job) -> bool {
        job.data
            .as_ref()
            .map(|d| self.is_run_after_job_data(d))
            .unwrap_or(false)
    }
}

pub struct InfraConfigModule {
    pub redis_config: Option<RedisConfig>,
    pub rdb_config: Option<RdbConfig>,
    pub job_queue_config: Arc<JobQueueConfig>,
}

impl InfraConfigModule {
    pub fn new_by_env() -> Self {
        Self {
            redis_config: load_redis_config_from_env().ok(),
            rdb_config: load_db_config_from_env().ok(),
            job_queue_config: Arc::new(load_job_queue_config_from_env().unwrap()),
        }
    }
}
// using from other test
#[cfg(any(test, feature = "test-utils"))]
pub mod test {
    use super::{InfraConfigModule, JobQueueConfig};
    use once_cell::sync::Lazy;
    use std::sync::Arc;

    pub static JOB_QUEUE_CONFIG: Lazy<JobQueueConfig> = Lazy::new(|| JobQueueConfig {
        expire_job_result_seconds: 60,
        fetch_interval: 1000,
    });

    #[cfg(feature = "mysql")]
    pub fn new_for_test_config_rdb() -> InfraConfigModule {
        use infra_utils::infra::test::{MYSQL_CONFIG, REDIS_CONFIG};

        InfraConfigModule {
            rdb_config: Some(MYSQL_CONFIG.clone()),
            redis_config: Some(REDIS_CONFIG.clone()),
            job_queue_config: Arc::new(JOB_QUEUE_CONFIG.clone()),
        }
    }
    #[cfg(not(feature = "mysql"))]
    pub fn new_for_test_config_rdb() -> InfraConfigModule {
        use infra_utils::infra::test::{REDIS_CONFIG, SQLITE_CONFIG};

        InfraConfigModule {
            rdb_config: Some(SQLITE_CONFIG.clone()),
            redis_config: Some(REDIS_CONFIG.clone()),
            job_queue_config: Arc::new(JOB_QUEUE_CONFIG.clone()),
        }
    }
}
