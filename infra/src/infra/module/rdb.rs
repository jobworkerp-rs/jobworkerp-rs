use crate::infra::job::rdb::{RdbJobRepositoryImpl, UseRdbJobRepository};
use crate::infra::job_result::rdb::{RdbJobResultRepositoryImpl, UseRdbJobResultRepository};
use crate::infra::worker::rdb::{RdbWorkerRepositoryImpl, UseRdbWorkerRepository};
use crate::infra::InfraConfigModule;

pub trait UseRdbRepositoryModule {
    fn rdb_repository_module(&self) -> &RdbRepositoryModule;
}
impl<T: UseRdbRepositoryModule> UseRdbWorkerRepository for T {
    fn rdb_worker_repository(&self) -> &RdbWorkerRepositoryImpl {
        &self.rdb_repository_module().worker_repository
    }
}
impl<T: UseRdbRepositoryModule> UseRdbJobRepository for T {
    fn rdb_job_repository(&self) -> &RdbJobRepositoryImpl {
        &self.rdb_repository_module().job_repository
    }
}
impl<T: UseRdbRepositoryModule> UseRdbJobResultRepository for T {
    fn rdb_job_result_repository(&self) -> &RdbJobResultRepositoryImpl {
        &self.rdb_repository_module().job_result_repository
    }
}

#[derive(Clone, Debug)]
pub struct RdbRepositoryModule {
    pub worker_repository: RdbWorkerRepositoryImpl,
    pub job_repository: RdbJobRepositoryImpl,
    pub job_result_repository: RdbJobResultRepositoryImpl,
}

impl RdbRepositoryModule {
    pub async fn new_by_env() -> Self {
        let pool = super::super::resource::setup_rdb_by_env().await;
        RdbRepositoryModule {
            worker_repository: RdbWorkerRepositoryImpl::new(pool),
            job_repository: RdbJobRepositoryImpl::new(pool),
            job_result_repository: RdbJobResultRepositoryImpl::new(pool),
        }
    }
    pub async fn new(config_module: &InfraConfigModule) -> Self {
        let pool =
            super::super::resource::setup_rdb(config_module.rdb_config.as_ref().unwrap()).await;
        RdbRepositoryModule {
            worker_repository: RdbWorkerRepositoryImpl::new(pool),
            job_repository: RdbJobRepositoryImpl::new(pool),
            job_result_repository: RdbJobResultRepositoryImpl::new(pool),
        }
    }
}

impl UseRdbRepositoryModule for RdbRepositoryModule {
    fn rdb_repository_module(&self) -> &RdbRepositoryModule {
        self
    }
}

pub mod test {
    use super::RdbRepositoryModule;
    use crate::infra::{
        job::rdb::RdbJobRepositoryImpl, job_result::rdb::RdbJobResultRepositoryImpl,
        worker::rdb::RdbWorkerRepositoryImpl,
    };
    use infra_utils::infra::test::{setup_test_mysql, TEST_RUNTIME};
    use sqlx::{Any, Executor, Pool};

    pub fn setup_test_rdb_module() -> RdbRepositoryModule {
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_mysql("../infra/sql/mysql").await;
            pool.execute("SELECT 1;").await.expect("test connection");
            truncate_tables(pool).await;
            RdbRepositoryModule {
                worker_repository: RdbWorkerRepositoryImpl::new(pool),
                job_repository: RdbJobRepositoryImpl::new(pool),
                job_result_repository: RdbJobResultRepositoryImpl::new(pool),
            }
        })
    }
    pub async fn truncate_tables(pool: &Pool<Any>) {
        sqlx::raw_sql("TRUNCATE TABLE job; TRUNCATE TABLE worker; TRUNCATE TABLE job_result;")
            .execute(pool)
            .await
            .expect("truncate all tables");
    }
}
