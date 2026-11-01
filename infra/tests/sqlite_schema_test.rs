use std::collections::BTreeMap;

use sqlx::{Row, SqlitePool, migrate::Migrator, sqlite::SqlitePoolOptions};

const DOCUMENTED_SCHEMA: &str = include_str!("../sql/sqlite/schema.sql");

#[tokio::test]
async fn sqlite_migrations_match_documented_schema() {
    let migrated_pool = migrated_sqlite_pool().await;

    let documented_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("documented-schema database should be available");
    sqlx::raw_sql(sqlx::AssertSqlSafe(DOCUMENTED_SCHEMA.to_owned()))
        .execute(&documented_pool)
        .await
        .expect("documented SQLite schema should be executable");

    assert_eq!(
        sqlite_schema_snapshot(&migrated_pool).await,
        sqlite_schema_snapshot(&documented_pool).await,
        "the documented schema must describe the result of all SQLite migrations"
    );
}

#[tokio::test]
async fn sqlite_migrations_remove_the_slack_builtin_runner() {
    let pool = migrated_sqlite_pool().await;

    let slack_runner_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runner WHERE id = 5 AND name = 'SLACK_POST_MESSAGE' AND type = 5",
    )
    .fetch_one(&pool)
    .await
    .expect("runner definitions should be queryable");

    assert_eq!(slack_runner_count, 0);
}

async fn migrated_sqlite_pool() -> SqlitePool {
    let migration_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sql/migrations/sqlite");
    let migrator = Migrator::new(migration_dir)
        .await
        .expect("SQLite migration directory should be readable");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("migration database should be available");
    migrator
        .run(&pool)
        .await
        .expect("SQLite migrations should apply to an empty database");
    pool
}

#[tokio::test]
async fn slack_removal_requires_all_runner_identity_fields_to_match() {
    for (description, fixture_sql, expected_runner_count) in [
        ("different id", "UPDATE runner SET id = 55 WHERE id = 5", 1),
        (
            "different name",
            "UPDATE runner SET name = 'CUSTOM_RUNNER' WHERE id = 5",
            1,
        ),
        (
            "different type",
            "UPDATE runner SET type = 55 WHERE id = 5",
            1,
        ),
        ("missing runner", "DELETE FROM runner WHERE id = 5", 0),
    ] {
        let pool = sqlite_pool_with_base_schema().await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(fixture_sql.to_owned()))
            .execute(&pool)
            .await
            .expect("runner fixture should be prepared");
        apply_slack_removal_migration(&pool).await;

        let runner_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM runner WHERE id = 5 OR id = 55")
                .fetch_one(&pool)
                .await
                .expect("runner definitions should be queryable");
        assert_eq!(
            runner_count, expected_runner_count,
            "the migration should preserve or tolerate the {description} fixture"
        );
    }
}

#[tokio::test]
async fn slack_removal_keeps_other_runners_workers_and_jobs() {
    let pool = sqlite_pool_with_base_schema().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        "INSERT INTO worker (
            id, name, description, runner_id, runner_settings, retry_type, interval,
            max_interval, max_retry, basis, periodic_interval, channel, queue_type,
            response_type, store_success, store_failure, use_static, broadcast_results
         ) VALUES (
            100, 'slack-worker', 'retained worker', 5, X'', 0, 0, 0, 0, 0, 0,
            NULL, 0, 0, 0, 0, 0, 0
         );
         INSERT INTO job (
            id, worker_id, args, enqueue_time, run_after_time, retried, priority,
            timeout, request_streaming
         ) VALUES (200, 100, X'', 0, 0, 0, 0, 0, 0);"
            .to_owned(),
    ))
    .execute(&pool)
    .await
    .expect("worker and job fixtures should be prepared");

    apply_slack_removal_migration(&pool).await;

    let retained_worker_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM worker WHERE id = 100")
            .fetch_one(&pool)
            .await
            .expect("worker definitions should be queryable");
    let retained_job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM job WHERE id = 200")
        .fetch_one(&pool)
        .await
        .expect("jobs should be queryable");
    let retained_command_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runner WHERE id = 1 AND name = 'COMMAND' AND type = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("runner definitions should be queryable");

    assert_eq!(retained_worker_count, 1);
    assert_eq!(retained_job_count, 1);
    assert_eq!(retained_command_count, 1);
}

async fn sqlite_pool_with_base_schema() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("fixture database should be available");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        include_str!("../sql/migrations/sqlite/001_base_schema.sql").to_owned(),
    ))
    .execute(&pool)
    .await
    .expect("base schema should create migration fixtures");
    pool
}

async fn apply_slack_removal_migration(pool: &SqlitePool) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        include_str!("../sql/migrations/sqlite/005_remove_slack_post_message_runner.sql")
            .to_owned(),
    ))
    .execute(pool)
    .await
    .expect("Slack runner removal migration should apply");
}

async fn sqlite_schema_snapshot(pool: &SqlitePool) -> BTreeMap<String, String> {
    let table_names = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations' \
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .expect("SQLite table metadata should be queryable");

    let mut snapshot = BTreeMap::new();
    for table_name in table_names {
        let quoted_table_name = table_name.replace('\'', "''");
        let columns = sqlx::query(sqlx::AssertSqlSafe(format!(
            "PRAGMA table_info('{}')",
            quoted_table_name
        )))
        .fetch_all(pool)
        .await
        .expect("SQLite column metadata should be queryable")
        .into_iter()
        .map(|row| {
            (
                row.get::<String, _>("name"),
                row.get::<String, _>("type"),
                row.get::<i64, _>("notnull"),
                row.get::<Option<String>, _>("dflt_value"),
                row.get::<i64, _>("pk"),
            )
        })
        .collect::<Vec<_>>();
        snapshot.insert(format!("table:{table_name}"), format!("{columns:?}"));

        let indexes = sqlx::query(sqlx::AssertSqlSafe(format!(
            "PRAGMA index_list('{}')",
            quoted_table_name
        )))
        .fetch_all(pool)
        .await
        .expect("SQLite index metadata should be queryable");
        for index in indexes {
            // SQLite creates implicit indexes for PRIMARY KEY and UNIQUE constraints.
            if index.get::<String, _>("origin") != "c" {
                continue;
            }
            let index_name = index.get::<String, _>("name");
            let index_sql = sqlx::query_scalar::<_, String>(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?",
            )
            .bind(&index_name)
            .fetch_one(pool)
            .await
            .expect("SQLite index definition should be queryable");
            snapshot.insert(
                format!("index:{table_name}:{index_name}"),
                format!(
                    "unique={},partial={},sql={index_sql:?}",
                    index.get::<i64, _>("unique"),
                    index.get::<i64, _>("partial")
                ),
            );
        }
    }

    let runners =
        sqlx::query("SELECT id, name, description, definition, type FROM runner ORDER BY id")
            .fetch_all(pool)
            .await
            .expect("initial runner definitions should be queryable");
    for runner in runners {
        let id = runner.get::<i64, _>("id");
        snapshot.insert(
            format!("runner:{id}"),
            format!(
                "name={:?},description={:?},definition={:?},type={}",
                runner.get::<String, _>("name"),
                runner.get::<String, _>("description"),
                runner.get::<String, _>("definition"),
                runner.get::<i64, _>("type")
            ),
        );
    }
    snapshot
}
