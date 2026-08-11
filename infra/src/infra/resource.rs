use anyhow::Result;
#[cfg(not(feature = "mysql"))]
use anyhow::{Context, bail};
use infra_utils::infra::{
    rdb::{RdbConfig, RdbConfigImpl, RdbPool, RdbUrlConfigImpl},
    redis::{RedisConfig, RedisPool},
};
use jobworkerp_base::error::JobWorkerError;

#[cfg(not(feature = "mysql"))]
use infra_utils::infra::rdb::Rdb;

#[cfg(not(feature = "mysql"))]
static SQLITE_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("sql/migrations/sqlite");

#[cfg(not(feature = "mysql"))]
const SQLITE_LEGACY_BASELINE_VERSION: i64 = 2;

#[cfg(not(feature = "mysql"))]
const SQLITE_REQUIRED_TABLES: &[&str] = &[
    "worker",
    "job",
    "job_result",
    "runner",
    "function_set",
    "function_set_target",
    "job_execution_overrides",
    "job_processing_status",
];

#[cfg(not(feature = "mysql"))]
const SQLITE_REQUIRED_COLUMNS: &[(&str, &str)] = &[
    ("worker", "created_at"),
    ("job", "using"),
    ("job_result", "using"),
    ("runner", "created_at"),
];

#[cfg(not(feature = "mysql"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqliteSchemaState {
    Pre,
    Post,
    Partial,
}

#[cfg(not(feature = "mysql"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationHistoryState {
    Unrecorded,
    Complete,
    Pending,
    Partial,
}

#[cfg(not(feature = "mysql"))]
fn classify_migration_history(applied: &[i64], expected: &[i64]) -> MigrationHistoryState {
    if applied.is_empty() {
        return MigrationHistoryState::Unrecorded;
    }

    let mut applied = applied.to_vec();
    let mut expected = expected.to_vec();
    applied.sort_unstable();
    expected.sort_unstable();
    if applied == expected {
        return MigrationHistoryState::Complete;
    }
    if !applied.is_empty()
        && applied.len() < expected.len()
        && applied.as_slice() == &expected[..applied.len()]
    {
        MigrationHistoryState::Pending
    } else {
        MigrationHistoryState::Partial
    }
}

#[cfg(not(feature = "mysql"))]
async fn inspect_sqlite_schema(pool: &RdbPool) -> Result<SqliteSchemaState> {
    let required_table_count = sqlx::query_scalar::<Rdb, i64>(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'table' AND name IN \
         ('worker', 'job', 'job_result', 'runner', 'function_set', \
          'function_set_target', 'job_execution_overrides', 'job_processing_status')",
    )
    .fetch_one(pool)
    .await
    .context("failed to inspect SQLite base tables")?;

    if required_table_count == 0 {
        return Ok(SqliteSchemaState::Pre);
    }
    if required_table_count != SQLITE_REQUIRED_TABLES.len() as i64 {
        return Ok(SqliteSchemaState::Partial);
    }

    for (table, column) in SQLITE_REQUIRED_COLUMNS {
        let exists = sqlx::query_scalar::<Rdb, i64>(
            "SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?",
        )
        .bind(table)
        .bind(column)
        .fetch_one(pool)
        .await
        .with_context(|| format!("failed to inspect SQLite column {table}.{column}"))?;
        if exists != 1 {
            return Ok(SqliteSchemaState::Partial);
        }
    }

    let recovery_column =
        sqlx::query_scalar::<Rdb, i64>("SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?")
            .bind("job_processing_status")
            .bind("worker_instance_id")
            .fetch_one(pool)
            .await
            .context("failed to inspect SQLite recovery column")?
            == 1;
    let recovery_index = sqlx::query_scalar::<Rdb, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
    )
    .bind("idx_jps_recovery_instance_running")
    .fetch_one(pool)
    .await
    .context("failed to inspect SQLite recovery index")?
        == 1;
    match (recovery_column, recovery_index) {
        (false, false) => Ok(SqliteSchemaState::Pre),
        (true, true) => Ok(SqliteSchemaState::Post),
        _ => Ok(SqliteSchemaState::Partial),
    }
}

#[cfg(not(feature = "mysql"))]
async fn load_sqlite_migration_history(pool: &RdbPool) -> Result<Vec<i64>> {
    let table_exists = sqlx::query_scalar::<Rdb, i64>(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .context("failed to inspect SQLite migration history")?;
    if table_exists == 0 {
        return Ok(Vec::new());
    }

    let rows = sqlx::query("SELECT version, success FROM _sqlx_migrations ORDER BY version")
        .fetch_all(pool)
        .await
        .context("failed to read SQLite migration history")?;
    let mut versions = Vec::with_capacity(rows.len());
    for row in rows {
        let version: i64 = sqlx::Row::try_get(&row, "version")
            .context("SQLite migration history has no version column")?;
        let success: i64 = sqlx::Row::try_get(&row, "success")
            .with_context(|| format!("failed to read SQLite migration result for {version}"))?;
        if success != 1 {
            bail!(
                "SQLite migration history contains failed migration {version}; repair the database before starting"
            );
        }
        versions.push(version);
    }
    Ok(versions)
}

#[cfg(not(feature = "mysql"))]
async fn run_sqlite_migrations(pool: &RdbPool) -> Result<()> {
    let schema_state = inspect_sqlite_schema(pool).await?;
    let applied_versions = load_sqlite_migration_history(pool).await?;
    let expected_versions: Vec<_> = SQLITE_MIGRATOR.iter().map(|m| m.version).collect();

    match classify_migration_history(&applied_versions, &expected_versions) {
        MigrationHistoryState::Complete => {
            if schema_state != SqliteSchemaState::Post {
                bail!(
                    "SQLite migration history is complete, but the database schema is {schema_state:?}; restore a consistent backup"
                );
            }
            SQLITE_MIGRATOR
                .run(pool)
                .await
                .context("failed to validate SQLite migrations")?;
        }
        MigrationHistoryState::Pending => {
            SQLITE_MIGRATOR
                .run(pool)
                .await
                .context("failed to apply pending SQLite migrations")?;
        }
        MigrationHistoryState::Partial => {
            bail!(
                "SQLite migration history is partial ({applied_versions:?}, expected {expected_versions:?}); repair the database before starting"
            );
        }
        MigrationHistoryState::Unrecorded => match schema_state {
            SqliteSchemaState::Pre => SQLITE_MIGRATOR
                .run(pool)
                .await
                .context("failed to apply SQLite migrations")?,
            SqliteSchemaState::Post => {
                SQLITE_MIGRATOR
                    .skip(pool, Some(SQLITE_LEGACY_BASELINE_VERSION))
                    .await
                    .context("failed to baseline the unrecorded SQLite schema")?;
                SQLITE_MIGRATOR
                    .run(pool)
                    .await
                    .context("failed to apply SQLite migrations after baselining")?;
            }
            SqliteSchemaState::Partial => {
                bail!(
                    "SQLite database has an unrecorded partial schema; automatic migration is unsafe, restore a consistent backup"
                );
            }
        },
    }
    Ok(())
}

static RDB_POOL: tokio::sync::OnceCell<RdbPool> = tokio::sync::OnceCell::const_new();

pub async fn setup_rdb_by_env() -> &'static RdbPool {
    let conf = load_db_config_from_env().unwrap_or(
        load_db_url_config_from_env().unwrap_or(RdbConfig::Separate(RdbConfigImpl::default())),
    );
    setup_rdb(&conf).await
}

// new rdb pool and store as static
// (if failed initializing, panic!)
pub async fn setup_rdb(db_config: &RdbConfig) -> &'static RdbPool {
    sqlx::any::install_default_drivers();
    RDB_POOL
        .get_or_init(|| async {
            let pool = infra_utils::infra::rdb::new_rdb_pool(db_config, None)
                .await
                .unwrap();
            #[cfg(not(feature = "mysql"))]
            run_sqlite_migrations(&pool)
                .await
                .expect("failed to initialize SQLite migrations");
            pool
        })
        .await
}

pub fn load_db_url_config_from_env() -> Result<RdbConfig> {
    // sqlite first
    envy::prefixed("SQLITE_")
        .from_env::<RdbUrlConfigImpl>()
        .map(RdbConfig::Url)
        .or_else(|_| {
            envy::prefixed("MYSQL_")
                .from_env::<RdbUrlConfigImpl>()
                .map(RdbConfig::Url)
        })
        .map_err(|e| {
            JobWorkerError::RuntimeError(format!("cannot read redis config from env: {e:?}")).into()
        })
}

pub fn load_db_config_from_env() -> Result<RdbConfig> {
    // sqlite first
    envy::prefixed("SQLITE_")
        .from_env::<RdbConfigImpl>()
        .map(RdbConfig::Separate)
        .or_else(|_| {
            envy::prefixed("MYSQL_")
                .from_env::<RdbConfigImpl>()
                .map(RdbConfig::Separate)
        })
        .map_err(|e| {
            JobWorkerError::RuntimeError(format!("cannot read redis config from env: {e:?}")).into()
        })
}

static _REDIS: tokio::sync::OnceCell<RedisPool> = tokio::sync::OnceCell::const_new();
static _REDIS_BLOCKING: tokio::sync::OnceCell<RedisPool> = tokio::sync::OnceCell::const_new();

pub async fn setup_redis_client(config: RedisConfig) -> redis::Client {
    redis::Client::open(config.url.clone())
        .unwrap_or_else(|_| panic!("cannot open redis client: config={:?}", config))
}

pub async fn setup_redis_client_by_env() -> redis::Client {
    let conf = load_redis_config_from_env().unwrap();
    redis::Client::open(conf.url.clone())
        .unwrap_or_else(|_| panic!("cannot open redis client: config={:?}", conf))
}

// static _REDIS_CON: tokio::sync::OnceCell<redis::aio::Connection> =
//     tokio::sync::OnceCell::const_new();
// pub async fn setup_redis_connection_by_env() -> &'static redis::aio::Connection {
//     let conf = _load_redis_config_from_env().unwrap();
//     setup_redis_connection(conf).await
// }
// pub async fn setup_redis_connection(config: RedisConfig) -> &'static redis::aio::Connection {
//     _REDIS_CON
//         .get_or_init(|| async {
//             common::infra::redis::new_redis_connection(config.clone())
//                 .await
//                 .expect(
//                     format!("cannot initiailize redis connection: config={:?}", config).as_str(),
//                 )
//         })
//         .await
// }

pub async fn setup_redis_pool_by_env() -> &'static RedisPool {
    let conf = load_redis_config_from_env().unwrap();
    setup_redis_pool(conf).await
}

pub async fn setup_redis_pool(config: RedisConfig) -> &'static RedisPool {
    _REDIS
        .get_or_init(|| async {
            infra_utils::infra::redis::new_redis_pool(config)
                .await
                .expect("failed to initialize redis pool")
        })
        .await
}

/// Setup a Redis pool for blocking operations like BLPOP.
/// This pool has response_timeout disabled to allow indefinite waiting.
pub async fn setup_redis_blocking_pool_by_env() -> &'static RedisPool {
    let mut conf = load_redis_config_from_env().unwrap();
    conf.blocking = true;
    setup_redis_blocking_pool(conf).await
}

/// Setup a Redis pool for blocking operations like BLPOP.
/// This pool has response_timeout disabled to allow indefinite waiting.
pub async fn setup_redis_blocking_pool(config: RedisConfig) -> &'static RedisPool {
    _REDIS_BLOCKING
        .get_or_init(|| async {
            let mut blocking_config = config;
            blocking_config.blocking = true;
            infra_utils::infra::redis::new_redis_pool(blocking_config)
                .await
                .expect("failed to initialize redis blocking pool")
        })
        .await
}

pub fn load_redis_config_from_env() -> Result<RedisConfig> {
    envy::prefixed("REDIS_")
        .from_env::<RedisConfig>()
        .map_err(|e| {
            JobWorkerError::RuntimeError(format!("cannot read redis config from env: {e:?}")).into()
        })
}

#[cfg(all(test, not(feature = "mysql")))]
mod tests {
    use super::{
        MigrationHistoryState, SQLITE_MIGRATOR, SqliteSchemaState, classify_migration_history,
        inspect_sqlite_schema, run_sqlite_migrations,
    };
    use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};

    async fn test_pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn empty_database_is_an_unrecorded_pre_schema() {
        let pool = test_pool().await;

        assert_eq!(
            inspect_sqlite_schema(&pool).await.unwrap(),
            SqliteSchemaState::Pre
        );
    }

    #[tokio::test]
    async fn base_database_without_recovery_changes_is_a_pre_schema() {
        let pool = test_pool().await;
        create_base_schema(&pool).await;

        assert_eq!(
            inspect_sqlite_schema(&pool).await.unwrap(),
            SqliteSchemaState::Pre
        );
    }

    #[tokio::test]
    async fn complete_database_is_an_unrecorded_post_schema() {
        let pool = test_pool().await;
        create_base_schema(&pool).await;
        sqlx::query("ALTER TABLE job_processing_status ADD COLUMN worker_instance_id BIGINT")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE INDEX idx_jps_recovery_instance_running \
             ON job_processing_status(worker_instance_id)",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(
            inspect_sqlite_schema(&pool).await.unwrap(),
            SqliteSchemaState::Post
        );
    }

    #[tokio::test]
    async fn recovery_column_without_recovery_index_is_a_partial_schema() {
        let pool = test_pool().await;
        create_base_schema(&pool).await;
        sqlx::query("ALTER TABLE job_processing_status ADD COLUMN worker_instance_id BIGINT")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            inspect_sqlite_schema(&pool).await.unwrap(),
            SqliteSchemaState::Partial
        );
    }

    #[tokio::test]
    async fn incomplete_database_is_a_partial_schema() {
        let pool = test_pool().await;
        sqlx::query("CREATE TABLE runner (created_at BIGINT)")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            inspect_sqlite_schema(&pool).await.unwrap(),
            SqliteSchemaState::Partial
        );
    }

    async fn create_base_schema(pool: &SqlitePool) {
        for statement in [
            "CREATE TABLE worker (created_at BIGINT)",
            "CREATE TABLE job (`using` TEXT)",
            "CREATE TABLE job_result (`using` TEXT)",
            "CREATE TABLE runner (created_at BIGINT)",
            "CREATE TABLE function_set (id BIGINT)",
            "CREATE TABLE function_set_target (id BIGINT)",
            "CREATE TABLE job_execution_overrides (job_id BIGINT)",
            "CREATE TABLE job_processing_status (status BIGINT)",
        ] {
            sqlx::query(statement).execute(pool).await.unwrap();
        }
    }

    #[test]
    fn migration_history_distinguishes_unrecorded_complete_and_partial() {
        assert_eq!(
            classify_migration_history(&[], &[1, 2]),
            MigrationHistoryState::Unrecorded
        );
        assert_eq!(
            classify_migration_history(&[1, 2], &[1, 2]),
            MigrationHistoryState::Complete
        );
        assert_eq!(
            classify_migration_history(&[1], &[1, 2]),
            MigrationHistoryState::Pending
        );
        assert_eq!(
            classify_migration_history(&[2], &[1, 2]),
            MigrationHistoryState::Partial
        );
    }

    #[tokio::test]
    async fn unrecorded_base_schema_runs_all_migrations_and_preserves_data() {
        let pool = test_pool().await;
        let base_schema = include_str!("../../sql/migrations/sqlite/001_base_schema.sql");
        sqlx::raw_sql(sqlx::AssertSqlSafe(base_schema.to_owned()))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO runner (id, name, description, definition, type) \
             VALUES (9000, 'TEST_RUNNER', 'test', 'test', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_sqlite_migrations(&pool).await.unwrap();

        let versions =
            sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(versions, vec![1, 2, 3]);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM runner WHERE id = 9000")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn unrecorded_complete_schema_is_baselined_then_adds_job_result_indexes() {
        let pool = test_pool().await;
        let base_schema = include_str!("../../sql/migrations/sqlite/001_base_schema.sql");
        let recovery_migration =
            include_str!("../../sql/migrations/sqlite/002_worker_instance_rdb_status_recovery.sql");
        sqlx::raw_sql(sqlx::AssertSqlSafe(base_schema.to_owned()))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(recovery_migration.to_owned()))
            .execute(&pool)
            .await
            .unwrap();

        run_sqlite_migrations(&pool).await.unwrap();

        let versions =
            sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(versions, vec![1, 2, 3]);
        let search_index_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'index' AND name IN (\
                'idx_job_result_status', \
                'idx_job_result_start_time', \
                'idx_job_result_end_time', \
                'idx_job_result_end_status', \
                'idx_job_result_worker_end'\
             )",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(search_index_count, 5);
    }

    #[tokio::test]
    async fn unrecorded_partial_schema_fails_with_a_repair_message() {
        let pool = test_pool().await;
        let base_schema = include_str!("../../sql/migrations/sqlite/001_base_schema.sql");
        sqlx::raw_sql(sqlx::AssertSqlSafe(base_schema.to_owned()))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE job_processing_status ADD COLUMN worker_instance_id BIGINT")
            .execute(&pool)
            .await
            .unwrap();

        let error = run_sqlite_migrations(&pool).await.unwrap_err();
        assert!(
            error.to_string().contains("unrecorded partial schema"),
            "unexpected migration error: {error:#}"
        );
    }

    #[tokio::test]
    async fn recorded_prefix_runs_only_pending_migrations() {
        let pool = test_pool().await;
        SQLITE_MIGRATOR.run_to(1, &pool).await.unwrap();

        run_sqlite_migrations(&pool).await.unwrap();

        let versions =
            sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(versions, vec![1, 2, 3]);
    }
}
