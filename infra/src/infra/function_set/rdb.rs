use super::rows::{FunctionSetRow, FunctionSetTargetRow};
use crate::infra::IdGeneratorWrapper;
use crate::infra::UseIdGenerator;
use anyhow::{Context, Result};
use async_trait::async_trait;
use infra_utils::infra::rdb::Rdb;
use infra_utils::infra::rdb::RdbPool;
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::function::data::{FunctionId, FunctionSet, FunctionSetData, FunctionSetId};
use sqlx::{Executor, Transaction};
use std::sync::Arc;

#[async_trait]
pub trait FunctionSetRepository: UseRdbPool + UseIdGenerator + Sync + Send {
    async fn create(
        &self,
        tx: &mut Transaction<'_, Rdb>,
        function_set: &FunctionSetData,
    ) -> Result<FunctionSetId> {
        let id: i64 = self.id_generator().generate_id()?;
        let res = sqlx::query::<Rdb>(
            "INSERT INTO `function_set` (
            `id`,
            `name`,
            `description`,
            `category`
            ) VALUES (?,?,?,?)",
        )
        .bind(id)
        .bind(&function_set.name)
        .bind(&function_set.description)
        .bind(function_set.category)
        .execute(&mut **tx)
        .await
        .map_err(JobWorkerError::DBError)?;

        if res.rows_affected() > 0 {
            let set_id = FunctionSetId { value: id };

            // Insert targets if there are any
            if !function_set.targets.is_empty() {
                self.create_targets(&mut *tx, &set_id, function_set.targets.clone())
                    .await?;
            }

            Ok(set_id)
        } else {
            // no record?
            Err(JobWorkerError::RuntimeError(format!(
                "Cannot insert function_set (logic error?): {function_set:?}"
            ))
            .into())
        }
    }

    async fn create_targets(
        &self,
        tx: &mut Transaction<'_, Rdb>,
        set_id: &FunctionSetId,
        targets: Vec<FunctionId>,
    ) -> Result<usize> {
        if targets.is_empty() {
            return Ok(0);
        }

        // Convert FunctionId to (target_id, target_type) pairs
        let target_pairs: Vec<(i64, i32)> = targets
            .iter()
            .filter_map(|fid| FunctionSetTargetRow::from_function_id(set_id.value, fid))
            .collect();

        if target_pairs.is_empty() {
            return Ok(0);
        }

        // Create query with the appropriate number of VALUE triplets
        let values_placeholder = "(?, ?, ?)".to_string();
        let values: Vec<String> =
            std::iter::repeat_n(values_placeholder, target_pairs.len()).collect();

        let query = format!(
            "INSERT INTO `function_set_target` (
            `set_id`,
            `target_id`,
            `target_type`
            ) VALUES {}",
            values.join(",")
        );

        // Start with the base query
        let mut q = sqlx::query::<Rdb>(&query);

        // Bind all parameters for each target
        for (target_id, target_type) in &target_pairs {
            q = q.bind(set_id.value).bind(target_id).bind(target_type);
        }

        // Execute the query
        let res = q
            .execute(&mut **tx)
            .await
            .map_err(JobWorkerError::DBError)?;

        Ok(res.rows_affected() as usize)
    }

    async fn update(
        &self,
        tx: &mut Transaction<'_, Rdb>,
        id: &FunctionSetId,
        function_set: &FunctionSetData,
    ) -> Result<bool> {
        // Update the main function_set table
        let updated = sqlx::query(
            "UPDATE `function_set` SET
            `name` = ?,
            `description` = ?,
            `category` = ?
            WHERE `id` = ?;",
        )
        .bind(&function_set.name)
        .bind(&function_set.description)
        .bind(function_set.category)
        .bind(id.value)
        .execute(&mut **tx)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(JobWorkerError::DBError)
        .context(format!("error in update function_set: id = {}", id.value))?;

        // Delete existing targets for this function set
        sqlx::query("DELETE FROM `function_set_target` WHERE `set_id` = ?;")
            .bind(id.value)
            .execute(&mut **tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context(format!(
                "error deleting function_set_targets: set_id = {}",
                id.value
            ))?;

        // Create new targets if there are any
        if !function_set.targets.is_empty() {
            self.create_targets(&mut *tx, id, function_set.targets.clone())
                .await?;
        }

        Ok(updated)
    }

    async fn delete(&self, id: &FunctionSetId) -> Result<bool> {
        self.delete_tx(self.db_pool(), id).await
    }

    async fn delete_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &FunctionSetId,
    ) -> Result<bool> {
        let del = sqlx::query::<Rdb>("DELETE FROM `function_set` WHERE `id` = ?;")
            .bind(id.value)
            .execute(tx)
            .await
            .map(|r| r.rows_affected() > 0)
            .map_err(JobWorkerError::DBError)?;
        Ok(del)
    }

    async fn find(&self, id: &FunctionSetId) -> Result<Option<FunctionSet>> {
        let pool = self.db_pool();
        let row = self.find_row_tx(pool, id).await?;

        if let Some(fs_row) = row {
            let targets = self.find_targets_by_set_id(pool, fs_row.id).await?;
            Ok(Some(fs_row.to_proto(targets)))
        } else {
            Ok(None)
        }
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<FunctionSet>> {
        let pool = self.db_pool();
        let row = self.find_row_by_name_tx(pool, name).await?;

        if let Some(fs_row) = row {
            let targets = self.find_targets_by_set_id(pool, fs_row.id).await?;
            Ok(Some(fs_row.to_proto(targets)))
        } else {
            Ok(None)
        }
    }

    async fn find_row_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        id: &FunctionSetId,
    ) -> Result<Option<FunctionSetRow>> {
        let function_set =
            sqlx::query_as::<Rdb, FunctionSetRow>("SELECT * FROM `function_set` WHERE `id` = ?;")
                .bind(id.value)
                .fetch_optional(tx)
                .await
                .map_err(JobWorkerError::DBError)
                .context(format!("error in find: id = {}", id.value))?;

        Ok(function_set)
    }

    async fn find_row_by_name_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        name: &str,
    ) -> Result<Option<FunctionSetRow>> {
        let function_set =
            sqlx::query_as::<Rdb, FunctionSetRow>("SELECT * FROM `function_set` WHERE `name` = ?;")
                .bind(name)
                .fetch_optional(tx)
                .await
                .map_err(JobWorkerError::DBError)
                .context(format!("error in find_by_name: name = {name}"))?;

        Ok(function_set)
    }

    async fn find_targets_by_set_id<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        set_id: i64,
    ) -> Result<Vec<super::rows::FunctionSetTargetRow>> {
        sqlx::query_as::<Rdb, super::rows::FunctionSetTargetRow>(
            "SELECT * FROM `function_set_target` WHERE `set_id` = ?;",
        )
        .bind(set_id)
        .fetch_all(tx)
        .await
        .map_err(JobWorkerError::DBError)
        .context(format!("error finding targets for set_id = {set_id}"))
    }

    async fn find_list(
        &self,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<Vec<FunctionSet>> {
        let pool = self.db_pool();
        let rows = self.find_row_list_tx(pool, limit, offset).await?;

        // For each function set, fetch its targets and convert to proto
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let targets = self.find_targets_by_set_id(pool, row.id).await?;
            result.push(row.to_proto(targets));
        }

        Ok(result)
    }

    async fn find_row_list_tx<'c, E: Executor<'c, Database = Rdb>>(
        &self,
        tx: E,
        limit: Option<&i32>,
        offset: Option<&i64>,
    ) -> Result<Vec<FunctionSetRow>> {
        if let Some(l) = limit {
            sqlx::query_as::<_, FunctionSetRow>(
                "SELECT * FROM `function_set` ORDER BY `id` DESC LIMIT ? OFFSET ?;",
            )
            .bind(l)
            .bind(offset.unwrap_or(&0i64))
            .fetch_all(tx)
        } else {
            // fetch all!
            sqlx::query_as::<_, FunctionSetRow>("SELECT * FROM `function_set` ORDER BY `id` DESC;")
                .fetch_all(tx)
        }
        .await
        .map_err(JobWorkerError::DBError)
        .context(format!("error in find_list: ({limit:?}, {offset:?})"))
    }

    async fn count_list_tx<'c, E: Executor<'c, Database = Rdb>>(&self, tx: E) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) as count FROM `function_set`;")
            .fetch_one(tx)
            .await
            .map_err(JobWorkerError::DBError)
            .context("error in count_list".to_string())
    }
}

#[derive(Clone, Debug)]
pub struct FunctionSetRepositoryImpl {
    id_generator: Arc<IdGeneratorWrapper>,
    pool: &'static RdbPool,
}

pub trait UseFunctionSetRepository {
    fn function_set_repository(&self) -> &FunctionSetRepositoryImpl;
}

impl FunctionSetRepositoryImpl {
    pub fn new(id_generator: Arc<IdGeneratorWrapper>, pool: &'static RdbPool) -> Self {
        Self { id_generator, pool }
    }
}

impl UseRdbPool for FunctionSetRepositoryImpl {
    fn db_pool(&self) -> &RdbPool {
        self.pool
    }
}

impl UseIdGenerator for FunctionSetRepositoryImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl FunctionSetRepository for FunctionSetRepositoryImpl {}

mod test {
    use super::FunctionSetRepository;
    use super::FunctionSetRepositoryImpl;
    use crate::infra::IdGeneratorWrapper;
    use anyhow::Context;
    use anyhow::Result;
    use infra_utils::infra::rdb::RdbPool;
    use infra_utils::infra::rdb::UseRdbPool;
    use proto::jobworkerp::data::{RunnerId, WorkerId};
    use proto::jobworkerp::function::data::{
        function_id, FunctionId, FunctionSet, FunctionSetData, RunnerUsing,
    };
    use std::sync::Arc;

    async fn _test_repository(pool: &'static RdbPool) -> Result<()> {
        let repository = FunctionSetRepositoryImpl::new(Arc::new(IdGeneratorWrapper::new()), pool);
        let db = repository.db_pool();
        let data = Some(FunctionSetData {
            name: "hoge1".to_string(),
            description: "hoge2".to_string(),
            category: 4,
            targets: vec![
                FunctionId {
                    id: Some(function_id::Id::RunnerUsing(RunnerUsing {
                        runner_id: Some(RunnerId { value: 10 }),
                        using: None,
                    })),
                },
                FunctionId {
                    id: Some(function_id::Id::WorkerId(WorkerId { value: 20 })),
                },
            ],
        });

        let mut tx = db.begin().await.context("error in test")?;
        let id = repository.create(&mut tx, &data.clone().unwrap()).await?;
        assert!(id.value > 0);
        tx.commit().await.context("error in test delete commit")?;

        let id1 = id;
        let expect = FunctionSet {
            id: Some(id1),
            data,
        };

        // find
        let found = repository.find(&id1).await?;
        assert_eq!(Some(&expect), found.as_ref());

        // update
        tx = db.begin().await.context("error in test")?;
        let update = FunctionSetData {
            name: "fuga1".to_string(),
            description: "fuga2".to_string(),
            category: 5,
            targets: vec![
                FunctionId {
                    id: Some(function_id::Id::RunnerUsing(RunnerUsing {
                        runner_id: Some(RunnerId { value: 30 }),
                        using: None,
                    })),
                },
                FunctionId {
                    id: Some(function_id::Id::WorkerId(WorkerId { value: 40 })),
                },
            ],
        };
        let updated = repository
            .update(&mut tx, &expect.id.unwrap(), &update)
            //            .upsert(&mut tx, &expect.id.clone().unwrap(), &update)
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

    #[test]
    fn run_test() -> Result<()> {
        use infra_utils::infra::test::setup_test_rdb_from;
        use infra_utils::infra::test::TEST_RUNTIME;
        TEST_RUNTIME.block_on(async {
            let rdb_pool = if cfg!(feature = "mysql") {
                let pool = setup_test_rdb_from("sql/mysql").await;
                sqlx::query("TRUNCATE TABLE function_set;")
                    .execute(pool)
                    .await?;
                pool
            } else {
                let pool = setup_test_rdb_from("sql/sqlite").await;
                sqlx::query("DELETE FROM function_set;")
                    .execute(pool)
                    .await?;
                pool
            };
            _test_repository(rdb_pool).await
        })
    }

    #[test]
    fn test_function_id_roundtrip_conversion() -> Result<()> {
        use crate::infra::function_set::rows::FunctionSetTargetRow;

        // Test Runner ID conversion
        let runner_function_id = FunctionId {
            id: Some(function_id::Id::RunnerUsing(RunnerUsing {
                runner_id: Some(RunnerId { value: 123 }),
                using: None,
            })),
        };

        let (target_id, target_type) =
            FunctionSetTargetRow::from_function_id(1, &runner_function_id)
                .expect("Should convert Runner FunctionId");

        assert_eq!(target_id, 123);
        assert_eq!(target_type, 0); // RUNNER_TYPE

        let target_row = FunctionSetTargetRow {
            id: 1,
            set_id: 1,
            target_id,
            target_type,
        };

        let converted_back = target_row.to_function_id();
        assert_eq!(converted_back, runner_function_id);

        // Test Worker ID conversion
        let worker_function_id = FunctionId {
            id: Some(function_id::Id::WorkerId(WorkerId { value: 456 })),
        };

        let (target_id, target_type) =
            FunctionSetTargetRow::from_function_id(2, &worker_function_id)
                .expect("Should convert Worker FunctionId");

        assert_eq!(target_id, 456);
        assert_eq!(target_type, 1); // WORKER_TYPE

        let target_row = FunctionSetTargetRow {
            id: 2,
            set_id: 2,
            target_id,
            target_type,
        };

        let converted_back = target_row.to_function_id();
        assert_eq!(converted_back, worker_function_id);

        Ok(())
    }

    #[test]
    fn test_function_id_none_handling() {
        use crate::infra::function_set::rows::FunctionSetTargetRow;

        // Test FunctionId with None id
        let none_function_id = FunctionId { id: None };

        let result = FunctionSetTargetRow::from_function_id(1, &none_function_id);
        assert!(
            result.is_none(),
            "Should return None for FunctionId with no id set"
        );
    }

    #[test]
    fn test_unknown_target_type_handling() {
        use crate::infra::function_set::rows::FunctionSetTargetRow;

        // Test unknown target_type
        let target_row = FunctionSetTargetRow {
            id: 1,
            set_id: 1,
            target_id: 999,
            target_type: 99, // Unknown type
        };

        let function_id = target_row.to_function_id();
        assert!(
            function_id.id.is_none(),
            "Should return FunctionId with None for unknown target_type"
        );
    }
}
