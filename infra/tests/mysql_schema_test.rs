#![cfg(feature = "mysql")]

use std::collections::BTreeMap;

use sqlx::{MySqlPool, Row, migrate::Migrator, mysql::MySqlPoolOptions};

const DOCUMENTED_SCHEMA: &str = include_str!("../sql/mysql/schema.sql");

#[tokio::test]
#[ignore = "requires an external MySQL database configured with TEST_MYSQL_URL"]
async fn mysql_migrations_match_documented_schema() {
    let database_url = std::env::var("TEST_MYSQL_URL")
        .unwrap_or_else(|_| "mysql://mysql:mysqlpw@127.0.0.1:3306/test".to_owned());
    let pool = MySqlPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("MySQL schema snapshot test requires an external MySQL database");

    reset_schema(&pool).await;
    let migration_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sql/migrations/mysql");
    Migrator::new(migration_dir)
        .await
        .expect("MySQL migration directory should be readable")
        .run(&pool)
        .await
        .expect("MySQL migrations should apply to an empty database");
    let migrated_snapshot = mysql_schema_snapshot(&pool).await;

    reset_schema(&pool).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(DOCUMENTED_SCHEMA.to_owned()))
        .execute(&pool)
        .await
        .expect("documented MySQL schema should be executable");
    let documented_snapshot = mysql_schema_snapshot(&pool).await;

    assert_eq!(
        migrated_snapshot, documented_snapshot,
        "the documented schema must describe the result of all MySQL migrations"
    );
}

async fn reset_schema(pool: &MySqlPool) {
    for table_name in [
        "_sqlx_migrations",
        "function_set_target",
        "function_set",
        "job_execution_overrides",
        "job_processing_status",
        "job_result",
        "job",
        "worker",
        "runner",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP TABLE IF EXISTS `{table_name}`"
        )))
        .execute(pool)
        .await
        .expect("MySQL schema snapshot database should be resettable");
    }
}

async fn mysql_schema_snapshot(pool: &MySqlPool) -> BTreeMap<String, String> {
    let table_names = sqlx::query_scalar::<_, String>(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = DATABASE() \
           AND table_type = 'BASE TABLE' \
           AND table_name <> '_sqlx_migrations' \
         ORDER BY table_name",
    )
    .fetch_all(pool)
    .await
    .expect("MySQL table metadata should be queryable");

    let mut snapshot = BTreeMap::new();
    for table_name in table_names {
        let columns = sqlx::query(
            "SELECT column_name AS name, column_type AS data_type, \
                    is_nullable AS nullable, column_default AS default_value, \
                    column_key AS key_type, extra AS extra_value \
             FROM information_schema.columns \
             WHERE table_schema = DATABASE() AND table_name = ? \
             ORDER BY ordinal_position",
        )
        .bind(&table_name)
        .fetch_all(pool)
        .await
        .expect("MySQL column metadata should be queryable")
        .into_iter()
        .map(|row| {
            (
                row.get::<String, _>("name"),
                row.get::<String, _>("data_type"),
                row.get::<String, _>("nullable"),
                row.get::<Option<String>, _>("default_value"),
                row.get::<String, _>("key_type"),
                row.get::<String, _>("extra_value"),
            )
        })
        .collect::<Vec<_>>();
        snapshot.insert(
            format!("table:{table_name}:columns"),
            format!("{columns:?}"),
        );

        let indexes = sqlx::query(
            "SELECT index_name AS name, non_unique AS is_non_unique, \
                    seq_in_index AS sequence_number, column_name AS column_name, \
                    collation AS collation, sub_part AS sub_part, \
                    index_type AS index_type, nullable AS nullable \
             FROM information_schema.statistics \
             WHERE table_schema = DATABASE() AND table_name = ? \
             ORDER BY index_name, seq_in_index",
        )
        .bind(&table_name)
        .fetch_all(pool)
        .await
        .expect("MySQL index metadata should be queryable")
        .into_iter()
        .map(|row| {
            (
                row.get::<String, _>("name"),
                row.get::<i64, _>("is_non_unique"),
                row.get::<u64, _>("sequence_number"),
                row.get::<String, _>("column_name"),
                row.get::<Option<String>, _>("collation"),
                row.get::<Option<i64>, _>("sub_part"),
                row.get::<String, _>("index_type"),
                row.get::<String, _>("nullable"),
            )
        })
        .collect::<Vec<_>>();
        snapshot.insert(
            format!("table:{table_name}:indexes"),
            format!("{indexes:?}"),
        );
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
