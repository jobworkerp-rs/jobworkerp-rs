use super::rows::WorkerRow;
use crate::infra::job::rows::UseJobqueueAndCodec;
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::rdb::{query_result, Rdb, RdbPool, UseRdbPool};
use itertools::Itertools;
use jobworkerp_base::{codec::UseProstCodec, error::JobWorkerError};
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use sqlx::Executor;

#[async_trait]
pub trait RdbWorkerRepository: UseRdbPool + UseJobqueueAndCodec + Sync + Send {
    async fn create<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        worker: &WorkerData,
    ) -> Result<WorkerId> {
        let rp = worker.retry_policy.as_ref();
        let res = sqlx::query::<Rdb>(
            "INSERT INTO worker (
            `name`,
            `description`,
            `runner_id`,
            `use_static`,
            `runner_settings`,
            `retry_type`,
            `interval`,
            `max_interval`,
            `max_retry`,
            `basis`,
            `periodic_interval`,
            `channel`,
            `queue_type`,
            `response_type`,
            `store_success`,
            `store_failure`,
            `broadcast_results`
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&worker.name)
        .bind(&worker.description)
        .bind(worker.runner_id.as_ref().map(|s| s.value).unwrap_or(0))
        .bind(worker.use_static)
        .bind(&worker.runner_settings)
        .bind(rp.map(|p| p.r#type).unwrap_or(0))
        .bind(rp.map(|p| p.interval as i64).unwrap_or(0))
        .bind(rp.map(|p| p.max_interval as i64).unwrap_or(0))
        .bind(rp.map(|p| p.max_retry as i64).unwrap_or(0))
        .bind(rp.map(|p| p.basis).unwrap_or(2.0))
        .bind(worker.periodic_interval as i64)
        .bind(
            worker
                .channel
                .as_ref()
                .unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()),
        )
        .bind(worker.queue_type)
        .bind(worker.response_type)
        .bind(worker.store_success)
        .bind(worker.store_failure)
        .bind(worker.broadcast_results)
        .execute(tx)
        .await
        .map_err(JobWorkerError::DBError)?;
        Ok(WorkerId {
            value: query_result::last_insert_id(res),
        })
    }

    async fn update<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &WorkerId,
        worker: &WorkerData,
    ) -> Result<bool> {
        let rp = worker.retry_policy.as_ref();
        sqlx::query(
            "UPDATE `worker` SET
            `name` = ?,
            `description` = ?,
            `runner_id` = ?,
            `use_static` = ?,
            `runner_settings` = ?,
            `retry_type` = ?,
            `interval` = ?,
            `max_interval` = ?,
            `max_retry` = ?,
            `basis` = ?,
            `periodic_interval` = ?,
            `channel` = ?,
            `queue_type` = ?,
            `response_type` = ?,
            `store_success` = ?,
            `store_failure` = ?,
            `broadcast_results` = ?
            WHERE `id` = ?;",
        )
        .bind(&worker.name)
        .bind(&worker.description)
        .bind(worker.runner_id.map(|s| s.value).unwrap_or(0))
        .bind(worker.use_static)
        .bind(&worker.runner_settings)
        .bind(rp.map(|p| p.r#type).unwrap_or(0))
        .bind(rp.map(|p| p.interval as i64).unwrap_or(0))
        .bind(rp.map(|p| p.max_interval as i64).unwrap_or(0))
        .bind(rp.map(|p| p.max_retry as i64).unwrap_or(0))
        .bind(rp.map(|p| p.basis).unwrap_or(2.0))
        .bind(worker.periodic_interval as i64)
        .bind(
            worker
                .channel
                .as_ref()
                .unwrap_or(&Self::DEFAULT_CHANNEL_NAME.to_string()),
        )
        .bind(worker.queue_type)
        .bind(worker.response_type)
        .bind(worker.store_success)
        .bind(worker.store_failure)
        .bind(worker.broadcast_results)
        .bind(id.value)
        .execute(tx)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(JobWorkerError::DBError)
        .context(format!("error in update: id = {}", id.value))
    }

    async fn delete(&self, id: &WorkerId) -> Result<bool> {
        self.delete_tx(self.db_pool(), id).await
    }

    async fn delete_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &WorkerId,
    ) -> Result<bool> {
        sqlx::query::<Rdb>("DELETE FROM worker WHERE id = ?;")
            .bind(id.value)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(|e| JobWorkerError::DBError(e).into())
    }

    // for test
    async fn delete_all(&self) -> Result<bool> {
        sqlx::query::<Rdb>("DELETE FROM worker;")
            .execute(self.db_pool())
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(|e| JobWorkerError::DBError(e).into())
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Worker>> {
        sqlx::query_as::<Rdb, WorkerRow>("SELECT * FROM worker WHERE name = ?;")
            .bind(name)
            .fetch_optional(self.db_pool())
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find: name = {}", name))
            .map(|r| r.and_then(|r2| r2.to_proto().ok()))
    }

    async fn find(&self, id: &WorkerId) -> Result<Option<Worker>> {
        self.find_row_tx(self.db_pool(), id)
            .await
            .map(|r| r.and_then(|r2| r2.to_proto().ok()))
    }

    async fn find_row_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &WorkerId,
    ) -> Result<Option<WorkerRow>> {
        sqlx::query_as::<Rdb, WorkerRow>("SELECT * FROM worker WHERE id = ?;")
            .bind(id.value)
            .fetch_optional(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find: id = {}", id.value))
    }

    async fn find_list(
        &self,
        runner_types: Vec<i32>,
        channel: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
    ) -> Result<Vec<Worker>> {
        self.find_row_list_tx(self.db_pool(), runner_types, channel, limit, offset)
            .await
            .map(|r| r.iter().flat_map(|r2| r2.to_proto().ok()).collect_vec())
    }

    // Find workers with specified runner types and channel, with optional limit and offset
    // If runner_types is empty, it will not filter by runner types.
    // If channel is None, it will not filter by channel.
    // If limit is None, ignore offset and limit.
    async fn find_row_list_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        runner_types: Vec<i32>,
        channel: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
    ) -> Result<Vec<WorkerRow>> {
        let mut conditions = Vec::new();

        // Add runner_types condition if not empty
        if !runner_types.is_empty() {
            let placeholders = runner_types
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");
            conditions.push(format!("runner_id IN ({})", placeholders));
        }

        // Add channel condition if specified
        if channel.is_some() {
            conditions.push("channel = ?".to_string());
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };

        // Build base query
        let base_sql = format!("SELECT * FROM worker{}", where_clause);
        let limit_clause = if let Some(l) = limit {
            format!(
                " ORDER BY id DESC LIMIT {} OFFSET {}",
                l,
                offset.unwrap_or(0)
            )
        } else {
            " ORDER BY id DESC".to_string()
        };

        let full_sql = format!("{}{}", base_sql, limit_clause);
        let mut query = sqlx::query_as::<_, WorkerRow>(&full_sql);

        // Bind parameters
        if !runner_types.is_empty() {
            for rt in runner_types.iter() {
                query = query.bind(*rt);
            }
        }

        if let Some(ref ch) = channel {
            query = query.bind(ch);
        }

        query
            .fetch_all(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find_list: ({:?}, {:?})", limit, offset))
    }

    async fn count(&self) -> Result<i64> {
        self.count_list_tx(self.db_pool()).await
    }
    async fn count_list_tx<'c, E: Executor<'c, Database = Rdb>>(&self, tx: E) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) as count FROM worker;")
            .fetch_one(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context("error in count_list".to_string())
    }
}

#[derive(Clone, Debug)]
pub struct RdbWorkerRepositoryImpl {
    pool: &'static RdbPool,
}

pub trait UseRdbWorkerRepository {
    fn rdb_worker_repository(&self) -> &RdbWorkerRepositoryImpl;
}

impl RdbWorkerRepositoryImpl {
    pub fn new(pool: &'static RdbPool) -> Self {
        Self { pool }
    }
}

impl UseRdbPool for RdbWorkerRepositoryImpl {
    fn db_pool(&self) -> &RdbPool {
        self.pool
    }
}
impl UseProstCodec for RdbWorkerRepositoryImpl {}
impl UseJobqueueAndCodec for RdbWorkerRepositoryImpl {}

impl RdbWorkerRepository for RdbWorkerRepositoryImpl {}

mod test {
    use crate::infra::job::rows::JobqueueAndCodec;
    use crate::infra::job::rows::UseJobqueueAndCodec;

    use super::RdbWorkerRepository;
    use super::RdbWorkerRepositoryImpl;
    use anyhow::Context;
    use anyhow::Result;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::rdb::UseRdbPool;
    use proto::jobworkerp::data::QueueType;
    use proto::jobworkerp::data::ResponseType;
    use proto::jobworkerp::data::RetryPolicy;
    use proto::jobworkerp::data::RunnerId;
    use proto::jobworkerp::data::RunnerType;
    use proto::jobworkerp::data::Worker;
    use proto::jobworkerp::data::WorkerData;
    use proto::TestRunnerSettings;

    async fn _test_repository(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);
        let db = repository.db_pool();
        let data = Some(WorkerData {
            name: "hoge1".to_string(),
            description: "hoge2".to_string(),
            runner_id: Some(RunnerId { value: 323 }),
            runner_settings: JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                name: "hoge".to_string(),
            }),
            retry_policy: Some(RetryPolicy {
                r#type: 5,
                interval: 6,
                max_interval: 7,
                max_retry: 8,
                basis: 9.0,
            }),
            periodic_interval: 11,
            channel: Some("hoge10".to_string()),
            queue_type: QueueType::ForcedRdb as i32,
            response_type: ResponseType::NoResult as i32,
            store_success: true,
            store_failure: true,
            use_static: false,
            broadcast_results: false,
        });

        let mut tx = db.begin().await.context("error in test")?;
        let id = repository.create(&mut *tx, &data.clone().unwrap()).await?;
        assert!(id.value > 0);
        tx.commit().await.context("error in test delete commit")?;

        let id1 = id;
        let expect = Worker {
            id: Some(id1),
            data,
        };

        // find
        let found = repository.find(&id1).await?;
        assert_eq!(Some(&expect), found.as_ref());

        // update
        tx = db.begin().await.context("error in test")?;
        let update = WorkerData {
            name: "fuga1".to_string(),
            description: "fuga2".to_string(),
            runner_id: Some(RunnerId { value: 324 }),
            runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                name: "fuga".to_string(),
            }),
            retry_policy: Some(RetryPolicy {
                r#type: 6,
                interval: 7,
                max_interval: 8,
                max_retry: 9,
                basis: 10.0,
            }),
            periodic_interval: 12,
            channel: Some("hoge11".to_string()),
            queue_type: QueueType::Normal as i32,
            response_type: ResponseType::Direct as i32,
            store_success: false,
            store_failure: false,
            use_static: false,
            broadcast_results: false,
        };
        let updated = repository
            .update(&mut *tx, &expect.id.unwrap(), &update)
            .await?;
        assert!(updated);
        tx.commit().await.context("error in test delete commit")?;

        // find
        let found = repository.find(&expect.id.unwrap()).await?;
        assert_eq!(&update, &found.unwrap().data.unwrap());
        let count = repository.count_list_tx(repository.db_pool()).await?;
        assert_eq!(1, count);

        // delete record
        tx = db.begin().await.context("error in test")?;
        let del = repository.delete_tx(&mut *tx, &expect.id.unwrap()).await?;
        tx.commit().await.context("error in test delete commit")?;
        assert!(del, "delete error");
        Ok(())
    }

    async fn _test_find_list_repository(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);
        let db = repository.db_pool();

        // Create test data with different runner_types and channels
        let test_data = vec![
            WorkerData {
                name: "worker1".to_string(),
                description: "description1".to_string(),
                runner_id: Some(RunnerId { value: 2 }),
                runner_settings: JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                    name: "test1".to_string(),
                }),
                retry_policy: None,
                periodic_interval: 0,
                channel: Some("channel1".to_string()),
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: false,
                store_failure: false,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "worker2".to_string(),
                description: "description2".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                    name: "test2".to_string(),
                }),
                retry_policy: None,
                periodic_interval: 0,
                channel: Some("channel2".to_string()),
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: false,
                store_failure: false,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "worker3".to_string(),
                description: "description3".to_string(),
                runner_id: Some(RunnerId { value: 2 }),
                runner_settings: JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                    name: "test3".to_string(),
                }),
                retry_policy: None,
                periodic_interval: 0,
                channel: Some("channel1".to_string()),
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: false,
                store_failure: false,
                use_static: false,
                broadcast_results: false,
            },
        ];

        // Insert test data
        let mut worker_ids = Vec::new();
        for data in &test_data {
            let mut tx = db.begin().await.context("error in test")?;
            let id = repository.create(&mut *tx, data).await?;
            tx.commit().await.context("error in test commit")?;
            worker_ids.push(id);
        }

        // Test 1: Find all workers (no filters)
        let all_workers = repository.find_list(vec![], None, None, None).await?;
        assert_eq!(all_workers.len(), 3, "Should find all 3 workers");

        // Test 2: Filter by runner_type
        let runner_type_1_workers = repository
            .find_list(vec![RunnerType::HttpRequest as i32], None, None, None)
            .await?;
        assert_eq!(
            runner_type_1_workers.len(),
            2,
            "Should find 2 workers with runner_type 1"
        );

        let runner_type_2_workers = repository
            .find_list(vec![RunnerType::Command as i32], None, None, None)
            .await?;
        assert_eq!(
            runner_type_2_workers.len(),
            1,
            "Should find 1 worker with runner_type 2"
        );

        // Test 3: Filter by channel
        let channel1_workers = repository
            .find_list(vec![], Some("channel1".to_string()), None, None)
            .await?;
        assert_eq!(
            channel1_workers.len(),
            2,
            "Should find 2 workers with channel1"
        );

        let channel2_workers = repository
            .find_list(vec![], Some("channel2".to_string()), None, None)
            .await?;
        assert_eq!(
            channel2_workers.len(),
            1,
            "Should find 1 worker with channel2"
        );

        // Test 4: Filter by both runner_type and channel
        let filtered_workers = repository
            .find_list(
                vec![RunnerType::HttpRequest as i32],
                Some("channel1".to_string()),
                None,
                None,
            )
            .await?;
        assert_eq!(
            filtered_workers.len(),
            2,
            "Should find 2 workers with runner_type 1 and channel1"
        );

        let filtered_workers_2 = repository
            .find_list(
                vec![RunnerType::Command as i32],
                Some("channel1".to_string()),
                None,
                None,
            )
            .await?;
        assert_eq!(
            filtered_workers_2.len(),
            0,
            "Should find 0 workers with runner_type 2 and channel1"
        );

        // Test 5: Test limit and offset
        let limited_workers = repository.find_list(vec![], None, Some(2), Some(0)).await?;
        assert_eq!(
            limited_workers.len(),
            2,
            "Should find 2 workers with limit 2"
        );

        let offset_workers = repository.find_list(vec![], None, Some(2), Some(1)).await?;
        assert_eq!(
            offset_workers.len(),
            2,
            "Should find 2 workers with limit 2 offset 1"
        );

        // Test 6: Multiple runner_types
        let multiple_runner_types = repository
            .find_list(
                vec![RunnerType::HttpRequest as i32, RunnerType::Command as i32],
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(
            multiple_runner_types.len(),
            3,
            "Should find all 3 workers with multiple runner_types"
        );

        // Test 7: Non-existent channel, type
        let no_workers = repository
            .find_list(
                vec![RunnerType::Docker as i32],
                Some("nonexistent".to_string()),
                None,
                None,
            )
            .await?;
        assert_eq!(
            no_workers.len(),
            0,
            "Should find 0 workers with non-existent channel"
        );

        // Clean up test data
        for id in worker_ids {
            let mut tx = db.begin().await.context("error in test")?;
            repository.delete_tx(&mut *tx, &id).await?;
            tx.commit().await.context("error in test delete commit")?;
        }

        Ok(())
    }

    #[test]
    fn run_test() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let rdb_pool = if cfg!(feature = "mysql") {
                let pool = setup_test_rdb_from("sql/mysql").await;
                sqlx::query("TRUNCATE TABLE worker;").execute(pool).await?;
                pool
            } else {
                let pool = setup_test_rdb_from("sql/sqlite").await;
                sqlx::query("DELETE FROM worker;").execute(pool).await?;
                pool
            };
            _test_repository(rdb_pool).await?;
            _test_find_list_repository(rdb_pool).await
        })
    }
}
