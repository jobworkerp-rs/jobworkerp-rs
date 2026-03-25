use anyhow::Result;
use infra_utils::infra::rdb::Rdb;
use proto::jobworkerp::data::{JobExecutionOverrides, JobId, RetryPolicy};
use sqlx::{Executor, FromRow};

#[derive(Debug, Clone, FromRow)]
pub struct JobExecutionOverridesRow {
    pub job_id: i64,
    pub response_type: Option<i32>,
    pub store_success: Option<bool>,
    pub store_failure: Option<bool>,
    pub broadcast_results: Option<bool>,
    pub retry_type: Option<i32>,
    pub retry_interval: Option<u32>,
    pub retry_max_interval: Option<u32>,
    pub retry_max_retry: Option<u32>,
    pub retry_basis: Option<f32>,
}

impl JobExecutionOverridesRow {
    pub fn to_proto(&self) -> JobExecutionOverrides {
        let retry_policy = if self.retry_type.is_some() {
            Some(RetryPolicy {
                r#type: self.retry_type.unwrap_or(0),
                interval: self.retry_interval.unwrap_or(0),
                max_interval: self.retry_max_interval.unwrap_or(0),
                max_retry: self.retry_max_retry.unwrap_or(0),
                basis: self.retry_basis.unwrap_or(0.0),
            })
        } else {
            None
        };
        JobExecutionOverrides {
            response_type: self.response_type,
            store_success: self.store_success,
            store_failure: self.store_failure,
            broadcast_results: self.broadcast_results,
            retry_policy,
        }
    }

    pub fn from_proto(job_id: i64, overrides: &JobExecutionOverrides) -> Self {
        let (retry_type, retry_interval, retry_max_interval, retry_max_retry, retry_basis) =
            if let Some(policy) = overrides.retry_policy.as_ref() {
                (
                    Some(policy.r#type),
                    Some(policy.interval),
                    Some(policy.max_interval),
                    Some(policy.max_retry),
                    Some(policy.basis),
                )
            } else {
                (None, None, None, None, None)
            };
        Self {
            job_id,
            response_type: overrides.response_type,
            store_success: overrides.store_success,
            store_failure: overrides.store_failure,
            broadcast_results: overrides.broadcast_results,
            retry_type,
            retry_interval,
            retry_max_interval,
            retry_max_retry,
            retry_basis,
        }
    }
}

/// Build comma-separated SQL bind placeholders for IN clauses.
/// e.g. count=3 → "?, ?, ?"
pub fn build_in_placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

pub async fn create_overrides_tx<'c, E: Executor<'c, Database = Rdb>>(
    tx: E,
    job_id: &JobId,
    overrides: &JobExecutionOverrides,
) -> Result<bool> {
    let row = JobExecutionOverridesRow::from_proto(job_id.value, overrides);
    let res = sqlx::query(
        "INSERT INTO job_execution_overrides (
            job_id, response_type, store_success, store_failure, broadcast_results,
            retry_type, retry_interval, retry_max_interval, retry_max_retry, retry_basis
        ) VALUES (?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(row.job_id)
    .bind(row.response_type)
    .bind(row.store_success)
    .bind(row.store_failure)
    .bind(row.broadcast_results)
    .bind(row.retry_type)
    .bind(row.retry_interval.map(|v| v as i64))
    .bind(row.retry_max_interval.map(|v| v as i64))
    .bind(row.retry_max_retry.map(|v| v as i64))
    .bind(row.retry_basis)
    .execute(tx)
    .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn find_overrides_tx<'c, E: Executor<'c, Database = Rdb>>(
    tx: E,
    job_id: &JobId,
) -> Result<Option<JobExecutionOverrides>> {
    let row = sqlx::query_as::<Rdb, JobExecutionOverridesRow>(
        "SELECT job_id, response_type, store_success, store_failure, broadcast_results,
                retry_type, retry_interval, retry_max_interval, retry_max_retry, retry_basis
         FROM job_execution_overrides WHERE job_id = ?",
    )
    .bind(job_id.value)
    .fetch_optional(tx)
    .await?;
    Ok(row.map(|r| r.to_proto()))
}

/// Batch-fetch overrides for multiple job IDs.
/// Splits into chunks of 500 to stay within SQLITE_MAX_VARIABLE_NUMBER (default: 999).
/// Returns a map from job_id to its overrides.
pub async fn find_overrides_batch_tx(
    pool: &infra_utils::infra::rdb::RdbPool,
    job_ids: &[i64],
) -> Result<std::collections::HashMap<i64, JobExecutionOverrides>> {
    if job_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    const CHUNK_SIZE: usize = 500;
    let mut result_map = std::collections::HashMap::new();
    for chunk in job_ids.chunks(CHUNK_SIZE) {
        let params = build_in_placeholders(chunk.len());
        let query_str = format!(
            "SELECT job_id, response_type, store_success, store_failure, broadcast_results,
                    retry_type, retry_interval, retry_max_interval, retry_max_retry, retry_basis
             FROM job_execution_overrides WHERE job_id IN ( {params} )"
        );
        let mut query = sqlx::query_as::<Rdb, JobExecutionOverridesRow>(&query_str);
        for id in chunk {
            query = query.bind(id);
        }
        let rows = query.fetch_all(pool).await?;
        result_map.extend(rows.into_iter().map(|r| (r.job_id, r.to_proto())));
    }
    Ok(result_map)
}

pub async fn delete_overrides_tx<'c, E: Executor<'c, Database = Rdb>>(
    tx: E,
    job_id: &JobId,
) -> Result<bool> {
    let res = sqlx::query("DELETE FROM job_execution_overrides WHERE job_id = ?")
        .bind(job_id.value)
        .execute(tx)
        .await?;
    Ok(res.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::data::{ResponseType, RetryPolicy, RetryType};

    fn full_overrides() -> JobExecutionOverrides {
        JobExecutionOverrides {
            response_type: Some(ResponseType::Direct as i32),
            store_success: Some(true),
            store_failure: Some(false),
            broadcast_results: Some(true),
            retry_policy: Some(RetryPolicy {
                r#type: RetryType::Exponential as i32,
                interval: 1000,
                max_interval: 60000,
                max_retry: 5,
                basis: 2.0,
            }),
        }
    }

    fn partial_overrides() -> JobExecutionOverrides {
        JobExecutionOverrides {
            response_type: Some(ResponseType::NoResult as i32),
            store_success: Some(false),
            store_failure: None,
            broadcast_results: None,
            retry_policy: None,
        }
    }

    async fn _test_create_find_delete_overrides(
        pool: &'static infra_utils::infra::rdb::RdbPool,
    ) -> anyhow::Result<()> {
        let job_id = JobId { value: 501 };

        let overrides = full_overrides();
        let created = create_overrides_tx(pool, &job_id, &overrides).await?;
        assert!(created);

        // find and verify all fields
        let found = find_overrides_tx(pool, &job_id).await?;
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.response_type, Some(ResponseType::Direct as i32));
        assert_eq!(found.store_success, Some(true));
        assert_eq!(found.store_failure, Some(false));
        assert_eq!(found.broadcast_results, Some(true));
        let policy = found.retry_policy.unwrap();
        assert_eq!(policy.r#type, RetryType::Exponential as i32);
        assert_eq!(policy.interval, 1000);
        assert_eq!(policy.max_interval, 60000);
        assert_eq!(policy.max_retry, 5);
        assert_eq!(policy.basis, 2.0);

        // delete
        let deleted = delete_overrides_tx(pool, &job_id).await?;
        assert!(deleted);

        // find returns None after delete
        let found_after = find_overrides_tx(pool, &job_id).await?;
        assert!(found_after.is_none());

        // delete non-existent returns false
        let deleted_again = delete_overrides_tx(pool, &job_id).await?;
        assert!(!deleted_again);

        Ok(())
    }

    async fn _test_find_overrides_batch(
        pool: &'static infra_utils::infra::rdb::RdbPool,
    ) -> anyhow::Result<()> {
        let ids = [601i64, 602, 603];

        // job 601: full overrides, job 602: partial overrides, job 603: no overrides
        create_overrides_tx(pool, &JobId { value: 601 }, &full_overrides()).await?;
        create_overrides_tx(pool, &JobId { value: 602 }, &partial_overrides()).await?;

        // batch fetch all 3
        let map = find_overrides_batch_tx(pool, &ids).await?;
        assert_eq!(map.len(), 2);
        assert!(map.contains_key(&601));
        assert!(map.contains_key(&602));
        assert!(!map.contains_key(&603));

        // verify values
        let o601 = &map[&601];
        assert_eq!(o601.response_type, Some(ResponseType::Direct as i32));
        assert!(o601.retry_policy.is_some());

        let o602 = &map[&602];
        assert_eq!(o602.response_type, Some(ResponseType::NoResult as i32));
        assert!(o602.retry_policy.is_none());

        // empty list returns empty map
        let empty = find_overrides_batch_tx(pool, &[]).await?;
        assert!(empty.is_empty());

        // cleanup overrides
        for id in &ids {
            delete_overrides_tx(pool, &JobId { value: *id }).await?;
        }
        Ok(())
    }

    #[cfg(not(feature = "mysql"))]
    #[test]
    fn test_sqlite_overrides_db() -> anyhow::Result<()> {
        use infra_utils::infra::test::TEST_RUNTIME;
        use infra_utils::infra::test::setup_test_rdb_from;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/sqlite").await;
            sqlx::query("DELETE FROM job_execution_overrides;")
                .execute(pool)
                .await?;
            sqlx::query("DELETE FROM job;").execute(pool).await?;
            _test_create_find_delete_overrides(pool).await?;
            _test_find_overrides_batch(pool).await
        })
    }

    #[cfg(feature = "mysql")]
    #[test]
    fn test_mysql_overrides_db() -> anyhow::Result<()> {
        use infra_utils::infra::test::TEST_RUNTIME;
        use infra_utils::infra::test::setup_test_rdb_from;
        TEST_RUNTIME.block_on(async {
            let pool = setup_test_rdb_from("sql/mysql").await;
            sqlx::query("DELETE FROM job_execution_overrides;")
                .execute(pool)
                .await?;
            sqlx::query("DELETE FROM job;").execute(pool).await?;
            _test_create_find_delete_overrides(pool).await?;
            _test_find_overrides_batch(pool).await
        })
    }

    #[test]
    fn test_row_roundtrip_full() {
        let overrides = JobExecutionOverrides {
            response_type: Some(ResponseType::Direct as i32),
            store_success: Some(true),
            store_failure: Some(false),
            broadcast_results: Some(true),
            retry_policy: Some(RetryPolicy {
                r#type: RetryType::Exponential as i32,
                interval: 1000,
                max_interval: 60000,
                max_retry: 5,
                basis: 2.0,
            }),
        };
        let row = JobExecutionOverridesRow::from_proto(42, &overrides);
        assert_eq!(row.job_id, 42);
        assert_eq!(row.response_type, Some(ResponseType::Direct as i32));
        assert_eq!(row.store_success, Some(true));
        assert_eq!(row.store_failure, Some(false));
        assert_eq!(row.broadcast_results, Some(true));
        assert_eq!(row.retry_type, Some(RetryType::Exponential as i32));
        assert_eq!(row.retry_interval, Some(1000));
        assert_eq!(row.retry_max_interval, Some(60000));
        assert_eq!(row.retry_max_retry, Some(5));
        assert_eq!(row.retry_basis, Some(2.0));

        let proto = row.to_proto();
        assert_eq!(proto.response_type, Some(ResponseType::Direct as i32));
        assert_eq!(proto.store_success, Some(true));
        assert_eq!(proto.store_failure, Some(false));
        assert_eq!(proto.broadcast_results, Some(true));
        let policy = proto.retry_policy.unwrap();
        assert_eq!(policy.r#type, RetryType::Exponential as i32);
        assert_eq!(policy.interval, 1000);
        assert_eq!(policy.max_interval, 60000);
        assert_eq!(policy.max_retry, 5);
        assert_eq!(policy.basis, 2.0);
    }

    #[test]
    fn test_row_roundtrip_none() {
        let overrides = JobExecutionOverrides {
            response_type: None,
            store_success: None,
            store_failure: None,
            broadcast_results: None,
            retry_policy: None,
        };
        let row = JobExecutionOverridesRow::from_proto(1, &overrides);
        assert!(row.response_type.is_none());
        assert!(row.store_success.is_none());
        assert!(row.retry_type.is_none());

        let proto = row.to_proto();
        assert!(proto.response_type.is_none());
        assert!(proto.store_success.is_none());
        assert!(proto.retry_policy.is_none());
    }

    #[test]
    fn test_row_roundtrip_partial() {
        let overrides = JobExecutionOverrides {
            response_type: Some(ResponseType::NoResult as i32),
            store_success: Some(true),
            store_failure: None,
            broadcast_results: None,
            retry_policy: None,
        };
        let row = JobExecutionOverridesRow::from_proto(10, &overrides);
        let proto = row.to_proto();
        assert_eq!(proto.response_type, Some(ResponseType::NoResult as i32));
        assert_eq!(proto.store_success, Some(true));
        assert!(proto.store_failure.is_none());
        assert!(proto.broadcast_results.is_none());
        assert!(proto.retry_policy.is_none());
    }
}
