use super::rows::WorkerRow;
use crate::infra::job::rows::UseJobqueueAndCodec;
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::rdb::{query_result, Rdb, RdbPool, UseRdbPool};
use itertools::Itertools;
use jobworkerp_base::{codec::UseProstCodec, error::JobWorkerError};
use proto::jobworkerp::data::{Worker, WorkerData, WorkerId};
use sqlx::{Database, Executor};

/// Represents a filter parameter binding for worker queries.
/// This enum provides type-safe binding for different parameter types
/// and ensures consistent handling across list and count queries.
#[derive(Debug)]
enum WorkerFilterBinding {
    I32(i32),
    I64(i64),
    String(String),
}

impl WorkerFilterBinding {
    /// Bind this parameter to a query that returns typed results (QueryAs)
    fn bind_query_as<'q, O>(
        &'q self,
        query: sqlx::query::QueryAs<'q, Rdb, O, <Rdb as Database>::Arguments<'q>>,
    ) -> sqlx::query::QueryAs<'q, Rdb, O, <Rdb as Database>::Arguments<'q>> {
        match self {
            WorkerFilterBinding::I32(value) => query.bind(*value),
            WorkerFilterBinding::I64(value) => query.bind(*value),
            WorkerFilterBinding::String(value) => query.bind(value.as_str()),
        }
    }

    /// Bind this parameter to a query that returns scalar values (QueryScalar)
    fn bind_query_scalar<'q, O>(
        &'q self,
        query: sqlx::query::QueryScalar<'q, Rdb, O, <Rdb as Database>::Arguments<'q>>,
    ) -> sqlx::query::QueryScalar<'q, Rdb, O, <Rdb as Database>::Arguments<'q>> {
        match self {
            WorkerFilterBinding::I32(value) => query.bind(*value),
            WorkerFilterBinding::I64(value) => query.bind(*value),
            WorkerFilterBinding::String(value) => query.bind(value.as_str()),
        }
    }
}

#[async_trait]
pub trait RdbWorkerRepository: UseRdbPool + UseJobqueueAndCodec + Sync + Send {
    async fn create<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        worker: &WorkerData,
    ) -> Result<WorkerId> {
        use command_utils::util::datetime;

        // Validate created_at timestamp before insertion (prevention of DEFAULT 0 issue)
        let created_at = datetime::now_millis();
        if created_at == 0 {
            return Err(JobWorkerError::InvalidParameter(
                "created_at must not be 0 (invalid timestamp)".to_string(),
            )
            .into());
        }

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
            `broadcast_results`,
            `created_at`
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
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
        .bind(created_at)
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
            .context(format!("error in find: name = {name}"))
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

    #[allow(clippy::too_many_arguments)]
    async fn find_list(
        &self,
        runner_types: Vec<i32>,
        channel: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
        name_filter: Option<String>,
        is_periodic: Option<bool>,
        runner_ids: Vec<i64>,
        sort_by: Option<proto::jobworkerp::data::WorkerSortField>,
        ascending: Option<bool>,
    ) -> Result<Vec<Worker>> {
        self.find_row_list_tx(
            self.db_pool(),
            runner_types,
            channel,
            limit,
            offset,
            name_filter,
            is_periodic,
            runner_ids,
            sort_by,
            ascending,
        )
        .await
        .map(|r| r.iter().flat_map(|r2| r2.to_proto().ok()).collect_vec())
    }

    // Find workers with specified filters, with optional limit and offset
    // If runner_types is empty, it will not filter by runner types.
    // If channel is None, it will not filter by channel.
    // If name_filter is None, it will not filter by name.
    // If is_periodic is None, it will not filter by periodic_interval.
    // If runner_ids is empty, it will not filter by runner_ids.
    // If sort_by is None, default to id DESC.
    // If limit is None, ignore offset and limit.
    #[allow(clippy::too_many_arguments)]
    async fn find_row_list_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        runner_types: Vec<i32>,
        channel: Option<String>,
        limit: Option<i32>,
        offset: Option<i64>,
        name_filter: Option<String>,
        is_periodic: Option<bool>,
        runner_ids: Vec<i64>,
        sort_by: Option<proto::jobworkerp::data::WorkerSortField>,
        ascending: Option<bool>,
    ) -> Result<Vec<WorkerRow>> {
        let (where_clause, bindings) = Self::build_worker_filters(
            &runner_types,
            &channel,
            &name_filter,
            is_periodic,
            &runner_ids,
        );

        // Build ORDER BY clause based on sort_by and ascending - convert enum to SQL field name
        use proto::jobworkerp::data::WorkerSortField;
        let sort_field = match sort_by {
            Some(WorkerSortField::Id) => "id",
            Some(WorkerSortField::Name) => "name",
            Some(WorkerSortField::RunnerId) => "runner_id",
            Some(WorkerSortField::PeriodicInterval) => "periodic_interval",
            Some(WorkerSortField::CreatedAt) => "created_at",
            _ => "id", // UNSPECIFIED or None -> default to id
        };
        let order = if ascending.unwrap_or(false) {
            "ASC"
        } else {
            "DESC"
        };
        let order_clause = format!(" ORDER BY {} {}", sort_field, order);

        // Build base query
        let base_sql = format!("SELECT * FROM worker{where_clause}{order_clause}");
        let limit_clause = if let Some(l) = limit {
            format!(" LIMIT {} OFFSET {}", l, offset.unwrap_or(0))
        } else {
            String::new()
        };

        let full_sql = format!("{base_sql}{limit_clause}");
        let query = sqlx::query_as::<_, WorkerRow>(&full_sql);
        let query = Self::bind_worker_filters_query_as(query, &bindings);

        query
            .fetch_all(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!("error in find_list: ({limit:?}, {offset:?})"))
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

    // Count workers with specified filters
    async fn count_by(
        &self,
        runner_types: Vec<i32>,
        channel: Option<String>,
        name_filter: Option<String>,
        is_periodic: Option<bool>,
        runner_ids: Vec<i64>,
    ) -> Result<i64> {
        self.count_by_tx(
            self.db_pool(),
            runner_types,
            channel,
            name_filter,
            is_periodic,
            runner_ids,
        )
        .await
    }

    async fn count_by_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        runner_types: Vec<i32>,
        channel: Option<String>,
        name_filter: Option<String>,
        is_periodic: Option<bool>,
        runner_ids: Vec<i64>,
    ) -> Result<i64> {
        let (where_clause, bindings) = Self::build_worker_filters(
            &runner_types,
            &channel,
            &name_filter,
            is_periodic,
            &runner_ids,
        );

        let sql = format!("SELECT count(*) as count FROM worker{where_clause};");
        let query = sqlx::query_scalar(&sql);
        let query = Self::bind_worker_filters_query_scalar(query, &bindings);

        query
            .fetch_one(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context("error in count_by".to_string())
    }

    /// Build WHERE clause and bindings for worker filters.
    /// This centralizes filter construction to ensure consistency between list and count queries.
    #[allow(private_interfaces)]
    fn build_worker_filters(
        runner_types: &[i32],
        channel: &Option<String>,
        name_filter: &Option<String>,
        is_periodic: Option<bool>,
        runner_ids: &[i64],
    ) -> (String, Vec<WorkerFilterBinding>) {
        let mut conditions = Vec::new();
        let mut bindings = Vec::new();

        // Add runner_types condition if not empty (for backward compatibility)
        // Use EXISTS subquery for better performance with large datasets:
        // - Utilizes primary key index (runner.id) for fast lookups
        // - Short-circuits on first match (SELECT 1)
        // - NULL-safe comparison
        // - Supports both built-in and user-created runners (MCP/Plugin)
        if !runner_types.is_empty() {
            let placeholders = runner_types
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM runner r WHERE r.id = worker.runner_id AND r.type IN ({placeholders}))"
            ));
            bindings.extend(runner_types.iter().copied().map(WorkerFilterBinding::I32));
        }

        // Add runner_ids condition if not empty (new filter)
        if !runner_ids.is_empty() {
            let placeholders = runner_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            conditions.push(format!("runner_id IN ({placeholders})"));
            bindings.extend(runner_ids.iter().copied().map(WorkerFilterBinding::I64));
        }

        // Add channel condition if specified
        if let Some(ch) = channel {
            conditions.push("channel = ?".to_string());
            bindings.push(WorkerFilterBinding::String(ch.clone()));
        }

        // Add name_filter condition if specified (prefix match)
        if let Some(name) = name_filter {
            conditions.push("name LIKE ?".to_string());
            use infra_utils::infra::rdb::escape::escape_like_pattern;
            let escaped = escape_like_pattern(name);
            bindings.push(WorkerFilterBinding::String(format!("{escaped}%")));
        }

        // Add is_periodic condition if specified
        if let Some(periodic) = is_periodic {
            if periodic {
                conditions.push("periodic_interval > 0".to_string());
            } else {
                conditions.push("periodic_interval = 0".to_string());
            }
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };

        (where_clause, bindings)
    }

    /// Bind filter parameters to a QueryAs query.
    #[allow(private_interfaces)]
    fn bind_worker_filters_query_as<'q, O>(
        mut query: sqlx::query::QueryAs<'q, Rdb, O, <Rdb as Database>::Arguments<'q>>,
        bindings: &'q [WorkerFilterBinding],
    ) -> sqlx::query::QueryAs<'q, Rdb, O, <Rdb as Database>::Arguments<'q>> {
        for binding in bindings {
            query = binding.bind_query_as(query);
        }
        query
    }

    /// Bind filter parameters to a QueryScalar query.
    #[allow(private_interfaces)]
    fn bind_worker_filters_query_scalar<'q, O>(
        mut query: sqlx::query::QueryScalar<'q, Rdb, O, <Rdb as Database>::Arguments<'q>>,
        bindings: &'q [WorkerFilterBinding],
    ) -> sqlx::query::QueryScalar<'q, Rdb, O, <Rdb as Database>::Arguments<'q>> {
        for binding in bindings {
            query = binding.bind_query_scalar(query);
        }
        query
    }

    // Count workers grouped by channel
    async fn count_by_channel(&self) -> Result<Vec<(String, i64)>> {
        self.count_by_channel_tx(self.db_pool()).await
    }

    async fn count_by_channel_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
    ) -> Result<Vec<(String, i64)>> {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT COALESCE(channel, '') as channel, count(*) as count
             FROM worker
             GROUP BY channel;",
        )
        .fetch_all(tx)
        .await
        .map_err(JobWorkerError::DBError)
        .context("error in count_by_channel".to_string())
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

#[cfg(test)]
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
            queue_type: QueueType::DbOnly as i32,
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
        let all_workers = repository
            .find_list(vec![], None, None, None, None, None, vec![], None, None)
            .await?;
        assert_eq!(all_workers.len(), 3, "Should find all 3 workers");

        // Test 2: Filter by runner_type
        let runner_type_1_workers = repository
            .find_list(
                vec![RunnerType::HttpRequest as i32],
                None,
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(
            runner_type_1_workers.len(),
            2,
            "Should find 2 workers with runner_type 1"
        );

        let runner_type_2_workers = repository
            .find_list(
                vec![RunnerType::Command as i32],
                None,
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(
            runner_type_2_workers.len(),
            1,
            "Should find 1 worker with runner_type 2"
        );

        // Test 3: Filter by channel
        let channel1_workers = repository
            .find_list(
                vec![],
                Some("channel1".to_string()),
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(
            channel1_workers.len(),
            2,
            "Should find 2 workers with channel1"
        );

        let channel2_workers = repository
            .find_list(
                vec![],
                Some("channel2".to_string()),
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
            )
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
                None,
                None,
                vec![],
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
                None,
                None,
                vec![],
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
        let limited_workers = repository
            .find_list(
                vec![],
                None,
                Some(2),
                Some(0),
                None,
                None,
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(
            limited_workers.len(),
            2,
            "Should find 2 workers with limit 2"
        );

        let offset_workers = repository
            .find_list(
                vec![],
                None,
                Some(2),
                Some(1),
                None,
                None,
                vec![],
                None,
                None,
            )
            .await?;
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
                None,
                None,
                vec![],
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
                None,
                None,
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(
            no_workers.len(),
            0,
            "Should find 0 workers with non-existent channel"
        );

        // Test 8: name_filter (prefix match)
        let name_filtered = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                Some("worker".to_string()),
                None,
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(
            name_filtered.len(),
            3,
            "Should find all 3 workers with name starting with 'worker'"
        );

        let name_filtered_2 = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                Some("worker2".to_string()),
                None,
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(
            name_filtered_2.len(),
            1,
            "Should find 1 worker with name starting with 'worker2'"
        );

        // Test 9: is_periodic filter
        // First, create a periodic worker
        let periodic_data = WorkerData {
            name: "periodic_worker".to_string(),
            description: "periodic test".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            runner_settings: JobqueueAndCodec::serialize_message(&TestRunnerSettings {
                name: "test_periodic".to_string(),
            }),
            retry_policy: None,
            periodic_interval: 60000, // 60 seconds
            channel: Some("channel1".to_string()),
            queue_type: QueueType::Normal as i32,
            response_type: ResponseType::NoResult as i32,
            store_success: false,
            store_failure: false,
            use_static: false,
            broadcast_results: false,
        };
        let mut tx = db.begin().await.context("error in test")?;
        let periodic_id = repository.create(&mut *tx, &periodic_data).await?;
        tx.commit().await.context("error in test commit")?;
        worker_ids.push(periodic_id);

        let periodic_workers = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                Some(true),
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(periodic_workers.len(), 1, "Should find 1 periodic worker");

        let non_periodic_workers = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                Some(false),
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(
            non_periodic_workers.len(),
            3,
            "Should find 3 non-periodic workers"
        );

        // Test 10: runner_ids filter
        let runner_id_filtered = repository
            .find_list(vec![], None, None, None, None, None, vec![1], None, None)
            .await?;
        assert_eq!(
            runner_id_filtered.len(),
            2,
            "Should find 2 workers with runner_id 1 (worker2 and periodic_worker)"
        );

        let runner_id_filtered_2 = repository
            .find_list(vec![], None, None, None, None, None, vec![2], None, None)
            .await?;
        assert_eq!(
            runner_id_filtered_2.len(),
            2,
            "Should find 2 workers with runner_id 2 (worker1 and worker3)"
        );

        // Test 11: Sort by name ASC
        let sorted_asc = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                None,
                vec![],
                Some(proto::jobworkerp::data::WorkerSortField::Name),
                Some(true),
            )
            .await?;
        assert_eq!(sorted_asc.len(), 4, "Should find all 4 workers");
        assert_eq!(
            sorted_asc[0].data.as_ref().unwrap().name,
            "periodic_worker",
            "First worker should be 'periodic_worker' (alphabetically)"
        );

        // Test 12: Sort by name DESC
        let sorted_desc = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                None,
                vec![],
                Some(proto::jobworkerp::data::WorkerSortField::Name),
                Some(false),
            )
            .await?;
        assert_eq!(sorted_desc.len(), 4, "Should find all 4 workers");
        assert_eq!(
            sorted_desc[0].data.as_ref().unwrap().name,
            "worker3",
            "First worker should be 'worker3' (reverse alphabetically)"
        );

        // Clean up test data
        for id in worker_ids {
            let mut tx = db.begin().await.context("error in test")?;
            repository.delete_tx(&mut *tx, &id).await?;
            tx.commit().await.context("error in test delete commit")?;
        }

        Ok(())
    }

    // Test EXISTS subquery optimization with runner_types filter
    async fn _test_runner_types_filter_with_exists(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);

        // Test that runner_types filter uses EXISTS subquery correctly
        // This test verifies that workers are filtered based on actual runner.type values
        // and that the EXISTS subquery properly joins worker.runner_id with runner.id

        // Count workers with runner_type = COMMAND (should use built-in runners from schema)
        let command_workers = repository
            .find_list(
                vec![RunnerType::Command as i32],
                None,
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
            )
            .await?;

        // Count workers with runner_type = HTTP_REQUEST
        let http_workers = repository
            .find_list(
                vec![RunnerType::HttpRequest as i32],
                None,
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
            )
            .await?;

        // Test count_by with runner_types filter (uses same EXISTS subquery)
        let command_count = repository
            .count_by(vec![RunnerType::Command as i32], None, None, None, vec![])
            .await?;

        let http_count = repository
            .count_by(
                vec![RunnerType::HttpRequest as i32],
                None,
                None,
                None,
                vec![],
            )
            .await?;

        // Verify that find_list and count_by return consistent results
        assert_eq!(
            command_workers.len() as i64,
            command_count,
            "find_list and count_by should return consistent results for COMMAND runners"
        );
        assert_eq!(
            http_workers.len() as i64,
            http_count,
            "find_list and count_by should return consistent results for HTTP_REQUEST runners"
        );

        // Test multiple runner_types (should use IN clause in EXISTS subquery)
        let multi_type_workers = repository
            .find_list(
                vec![RunnerType::Command as i32, RunnerType::HttpRequest as i32],
                None,
                None,
                None,
                None,
                None,
                vec![],
                None,
                None,
            )
            .await?;

        let multi_type_count = repository
            .count_by(
                vec![RunnerType::Command as i32, RunnerType::HttpRequest as i32],
                None,
                None,
                None,
                vec![],
            )
            .await?;

        assert_eq!(
            multi_type_workers.len() as i64,
            multi_type_count,
            "Multiple runner_types should work correctly with EXISTS subquery"
        );

        // Verify that the count matches the sum of individual counts
        assert!(
            multi_type_count >= command_count && multi_type_count >= http_count,
            "Multiple runner_types count should be >= individual counts"
        );

        Ok(())
    }

    // Test name_filter parameter (prefix match)
    async fn _test_find_list_by_name_filter(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);
        let db = repository.db_pool();

        // Clean up existing test data first
        sqlx::query("DELETE FROM worker WHERE name LIKE 'testprefix_%' OR name = 'other_worker';")
            .execute(db)
            .await
            .context("error in cleanup")?;

        // Create test workers with different name patterns
        let test_data = vec![
            WorkerData {
                name: "testprefix_alpha".to_string(),
                description: "Test 1".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings1".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "testprefix_beta".to_string(),
                description: "Test 2".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings2".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "other_worker".to_string(),
                description: "Other".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings3".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
        ];

        let mut worker_ids = Vec::new();
        for data in &test_data {
            let mut tx = db.begin().await.context("error in test")?;
            let id = repository.create(&mut *tx, data).await?;
            tx.commit().await.context("error in test commit")?;
            worker_ids.push(id);
        }

        // Test prefix match: "testprefix" should match "testprefix_alpha" and "testprefix_beta"
        let results = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                Some("testprefix".to_string()),
                None,
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(
            results.len(),
            2,
            "Should find 2 workers with name starting with 'testprefix'"
        );

        // Test count_by with name_filter
        let count = repository
            .count_by(vec![], None, Some("testprefix".to_string()), None, vec![])
            .await?;
        assert_eq!(
            count, 2,
            "count_by should return 2 workers with name starting with 'testprefix'"
        );

        // Cleanup
        for id in worker_ids {
            let mut tx = db.begin().await.context("error in test")?;
            repository.delete_tx(&mut *tx, &id).await?;
            tx.commit().await.context("error in test delete commit")?;
        }
        Ok(())
    }

    // Test is_periodic parameter
    async fn _test_find_list_by_is_periodic(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);
        let db = repository.db_pool();

        // Clean up existing test data first
        sqlx::query("DELETE FROM worker WHERE name LIKE 'periodic_worker_%' OR name = 'non_periodic_worker';")
            .execute(db)
            .await
            .context("error in cleanup")?;

        // Create test workers: 2 periodic, 1 non-periodic
        let test_data = vec![
            WorkerData {
                name: "periodic_worker_1".to_string(),
                description: "Periodic 1".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings1".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 60000, // 1 minute
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "periodic_worker_2".to_string(),
                description: "Periodic 2".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings2".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 300000, // 5 minutes
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "non_periodic_worker".to_string(),
                description: "Non-periodic".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings3".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0, // Non-periodic
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
        ];

        let mut worker_ids = Vec::new();
        for data in &test_data {
            let mut tx = db.begin().await.context("error in test")?;
            let id = repository.create(&mut *tx, data).await?;
            tx.commit().await.context("error in test commit")?;
            worker_ids.push(id);
        }

        // Test is_periodic = true (periodic workers only)
        let results = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                Some(true),
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 2, "Should find 2 periodic workers");

        // Test is_periodic = false (non-periodic workers only)
        let results = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                Some(false),
                vec![],
                None,
                None,
            )
            .await?;
        assert_eq!(results.len(), 1, "Should find 1 non-periodic worker");

        // Test is_periodic = None (all workers)
        let results = repository
            .find_list(vec![], None, None, None, None, None, vec![], None, None)
            .await?;
        assert_eq!(results.len(), 3, "Should find all 3 workers");

        // Test count_by with is_periodic
        let count = repository
            .count_by(vec![], None, None, Some(true), vec![])
            .await?;
        assert_eq!(count, 2, "count_by should return 2 periodic workers");

        let count = repository
            .count_by(vec![], None, None, Some(false), vec![])
            .await?;
        assert_eq!(count, 1, "count_by should return 1 non-periodic worker");

        // Cleanup
        for id in worker_ids {
            let mut tx = db.begin().await.context("error in test")?;
            repository.delete_tx(&mut *tx, &id).await?;
            tx.commit().await.context("error in test delete commit")?;
        }
        Ok(())
    }

    // Test runner_ids parameter
    async fn _test_find_list_by_runner_ids(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);
        let db = repository.db_pool();

        // Clean up existing test data first
        sqlx::query("DELETE FROM worker WHERE name LIKE 'worker_runner_%';")
            .execute(db)
            .await
            .context("error in cleanup")?;

        // Create test workers with different runner_ids
        let test_data = vec![
            WorkerData {
                name: "worker_runner_1".to_string(),
                description: "Runner 1".to_string(),
                runner_id: Some(RunnerId { value: 100 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings1".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "worker_runner_2".to_string(),
                description: "Runner 2".to_string(),
                runner_id: Some(RunnerId { value: 200 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings2".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "worker_runner_3".to_string(),
                description: "Runner 3".to_string(),
                runner_id: Some(RunnerId { value: 300 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings3".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
        ];

        let mut worker_ids = Vec::new();
        for data in &test_data {
            let mut tx = db.begin().await.context("error in test")?;
            let id = repository.create(&mut *tx, data).await?;
            tx.commit().await.context("error in test commit")?;
            worker_ids.push(id);
        }

        // Test single runner_id
        let results = repository
            .find_list(vec![], None, None, None, None, None, vec![100], None, None)
            .await?;
        assert_eq!(results.len(), 1, "Should find 1 worker with runner_id 100");

        // Test multiple runner_ids
        let results = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                None,
                vec![100, 200],
                None,
                None,
            )
            .await?;
        assert_eq!(
            results.len(),
            2,
            "Should find 2 workers with runner_ids 100 or 200"
        );

        // Test count_by with runner_ids
        let count = repository
            .count_by(vec![], None, None, None, vec![100, 200])
            .await?;
        assert_eq!(
            count, 2,
            "count_by should return 2 workers with runner_ids 100 or 200"
        );

        // Test empty runner_ids (should return all)
        let results = repository
            .find_list(vec![], None, None, None, None, None, vec![], None, None)
            .await?;
        assert_eq!(
            results.len(),
            3,
            "Empty runner_ids should return all workers"
        );

        // Cleanup
        for id in worker_ids {
            let mut tx = db.begin().await.context("error in test")?;
            repository.delete_tx(&mut *tx, &id).await?;
            tx.commit().await.context("error in test delete commit")?;
        }
        Ok(())
    }

    // Test sort_by and ascending parameters
    async fn _test_find_list_by_sort(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);
        let db = repository.db_pool();

        // Clean up existing test data first
        sqlx::query("DELETE FROM worker WHERE name IN ('charlie', 'alice', 'bob');")
            .execute(db)
            .await
            .context("error in cleanup")?;

        // Create test workers with different names and periodic_intervals
        let test_data = vec![
            WorkerData {
                name: "charlie".to_string(),
                description: "Third".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings1".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 300000, // 5 minutes
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "alpha".to_string(),
                description: "First".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings2".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 60000, // 1 minute
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "bravo".to_string(),
                description: "Second".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings3".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 180000, // 3 minutes
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
        ];

        let mut worker_ids = Vec::new();
        for data in &test_data {
            let mut tx = db.begin().await.context("error in test")?;
            let id = repository.create(&mut *tx, data).await?;
            tx.commit().await.context("error in test commit")?;
            worker_ids.push(id);
        }

        // Test sort by NAME ascending
        let results = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                None,
                vec![],
                Some(proto::jobworkerp::data::WorkerSortField::Name),
                Some(true),
            )
            .await?;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].data.as_ref().unwrap().name, "alpha");
        assert_eq!(results[1].data.as_ref().unwrap().name, "bravo");
        assert_eq!(results[2].data.as_ref().unwrap().name, "charlie");

        // Test sort by NAME descending
        let results = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                None,
                vec![],
                Some(proto::jobworkerp::data::WorkerSortField::Name),
                Some(false),
            )
            .await?;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].data.as_ref().unwrap().name, "charlie");
        assert_eq!(results[1].data.as_ref().unwrap().name, "bravo");
        assert_eq!(results[2].data.as_ref().unwrap().name, "alpha");

        // Test sort by PERIODIC_INTERVAL ascending
        let results = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                None,
                vec![],
                Some(proto::jobworkerp::data::WorkerSortField::PeriodicInterval),
                Some(true),
            )
            .await?;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].data.as_ref().unwrap().periodic_interval, 60000);
        assert_eq!(results[1].data.as_ref().unwrap().periodic_interval, 180000);
        assert_eq!(results[2].data.as_ref().unwrap().periodic_interval, 300000);

        // Test sort by PERIODIC_INTERVAL descending
        let results = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                None,
                None,
                vec![],
                Some(proto::jobworkerp::data::WorkerSortField::PeriodicInterval),
                Some(false),
            )
            .await?;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].data.as_ref().unwrap().periodic_interval, 300000);
        assert_eq!(results[1].data.as_ref().unwrap().periodic_interval, 180000);
        assert_eq!(results[2].data.as_ref().unwrap().periodic_interval, 60000);

        // Cleanup
        for id in worker_ids {
            let mut tx = db.begin().await.context("error in test")?;
            repository.delete_tx(&mut *tx, &id).await?;
            tx.commit().await.context("error in test delete commit")?;
        }
        Ok(())
    }

    // Test combined filters
    async fn _test_find_list_by_combined_filters(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);
        let db = repository.db_pool();

        // Clean up existing test data first
        sqlx::query("DELETE FROM worker WHERE name LIKE 'testprefix_periodic_%' OR name = 'other_worker_nonperiodic';")
            .execute(db)
            .await
            .context("error in cleanup")?;

        // Create test workers with various combinations
        let test_data = vec![
            WorkerData {
                name: "testprefix_periodic_100".to_string(),
                description: "Test 1".to_string(),
                runner_id: Some(RunnerId { value: 100 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings1".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 60000,
                channel: Some("channel1".to_string()),
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "testprefix_periodic_200".to_string(),
                description: "Test 2".to_string(),
                runner_id: Some(RunnerId { value: 200 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings2".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 120000,
                channel: Some("channel1".to_string()),
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "testprefix_non_periodic_100".to_string(),
                description: "Other".to_string(),
                runner_id: Some(RunnerId { value: 100 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings3".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: Some("channel2".to_string()),
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
        ];

        let mut worker_ids = Vec::new();
        for data in &test_data {
            let mut tx = db.begin().await.context("error in test")?;
            let id = repository.create(&mut *tx, data).await?;
            tx.commit().await.context("error in test commit")?;
            worker_ids.push(id);
        }

        // Test combined: name_filter + is_periodic + runner_ids
        let results = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                Some("testprefix".to_string()),
                Some(true),
                vec![100, 200],
                None,
                None,
            )
            .await?;
        assert_eq!(
            results.len(),
            2,
            "Should find 2 workers matching all conditions"
        );

        // Test combined with count_by
        let count = repository
            .count_by(
                vec![],
                None,
                Some("testprefix".to_string()),
                Some(true),
                vec![100, 200],
            )
            .await?;
        assert_eq!(count, 2, "count_by should match find_list result");

        // Test combined: name_filter + channel + runner_ids
        let results = repository
            .find_list(
                vec![],
                Some("channel1".to_string()),
                None,
                None,
                Some("testprefix".to_string()),
                None,
                vec![100],
                None,
                None,
            )
            .await?;
        assert_eq!(
            results.len(),
            1,
            "Should find 1 worker matching name_filter + channel + runner_ids"
        );

        // Test combined: name_filter + is_periodic + sort
        let results = repository
            .find_list(
                vec![],
                None,
                None,
                None,
                Some("testprefix".to_string()),
                Some(true),
                vec![],
                Some(proto::jobworkerp::data::WorkerSortField::PeriodicInterval),
                Some(true),
            )
            .await?;
        assert_eq!(results.len(), 2);
        assert!(
            results[0].data.as_ref().unwrap().periodic_interval
                < results[1].data.as_ref().unwrap().periodic_interval,
            "Results should be sorted by periodic_interval ascending"
        );

        // Cleanup
        for id in worker_ids {
            let mut tx = db.begin().await.context("error in test")?;
            repository.delete_tx(&mut *tx, &id).await?;
            tx.commit().await.context("error in test delete commit")?;
        }
        Ok(())
    }

    // Test count_by_channel
    async fn _test_count_by_channel(pool: &'static RdbPool) -> Result<()> {
        let repository = RdbWorkerRepositoryImpl::new(pool);
        let db = repository.db_pool();

        // Clean up ALL existing test data first (to ensure clean state)
        sqlx::query("DELETE FROM worker;")
            .execute(db)
            .await
            .context("error in cleanup")?;

        // Create test workers in different channels
        let test_data = vec![
            WorkerData {
                name: "worker_channel1_a".to_string(),
                description: "Channel 1 Worker A".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings1".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: Some("channel1".to_string()),
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "worker_channel1_b".to_string(),
                description: "Channel 1 Worker B".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings2".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: Some("channel1".to_string()),
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "worker_channel2".to_string(),
                description: "Channel 2 Worker".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings3".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: Some("channel2".to_string()),
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
            WorkerData {
                name: "worker_default".to_string(),
                description: "Default Channel Worker".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings: RdbWorkerRepositoryImpl::serialize_message(&TestRunnerSettings {
                    name: "settings4".to_string(),
                }),
                retry_policy: Some(RetryPolicy {
                    r#type: 1,
                    interval: 100,
                    max_interval: 1000,
                    max_retry: 3,
                    basis: 2.0,
                }),
                periodic_interval: 0,
                channel: None, // Default channel
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                broadcast_results: false,
            },
        ];

        let mut worker_ids = Vec::new();
        for data in &test_data {
            let mut tx = db.begin().await.context("error in test")?;
            let id = repository.create(&mut *tx, data).await?;
            tx.commit().await.context("error in test commit")?;
            worker_ids.push(id);
        }

        // Test count_by_channel - returns Vec<(channel_name, count)>
        let counts = repository.count_by_channel().await?;

        // Find count for channel1
        let channel1_count = counts
            .iter()
            .find(|(ch, _)| ch == "channel1")
            .map(|(_, count)| *count)
            .unwrap_or(0);
        assert_eq!(channel1_count, 2, "Should count 2 workers in channel1");

        // Find count for channel2
        let channel2_count = counts
            .iter()
            .find(|(ch, _)| ch == "channel2")
            .map(|(_, count)| *count)
            .unwrap_or(0);
        assert_eq!(channel2_count, 1, "Should count 1 worker in channel2");

        // Find count for default channel ("__default_job_channel__")
        let default_count = counts
            .iter()
            .find(|(ch, _)| ch == JobqueueAndCodec::DEFAULT_CHANNEL_NAME)
            .map(|(_, count)| *count)
            .unwrap_or(0);
        assert_eq!(default_count, 1, "Should count 1 worker in default channel");

        // Cleanup
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
            _test_find_list_repository(rdb_pool).await?;
            _test_runner_types_filter_with_exists(rdb_pool).await?;
            // Sprint 1-2 new parameter tests
            _test_find_list_by_name_filter(rdb_pool).await?;
            _test_find_list_by_is_periodic(rdb_pool).await?;
            _test_find_list_by_runner_ids(rdb_pool).await?;
            _test_find_list_by_sort(rdb_pool).await?;
            _test_find_list_by_combined_filters(rdb_pool).await?;
            _test_count_by_channel(rdb_pool).await
        })
    }
}
