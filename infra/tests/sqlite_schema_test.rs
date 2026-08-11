use std::collections::BTreeMap;

use sqlx::{Row, SqlitePool, migrate::Migrator, sqlite::SqlitePoolOptions};

const DOCUMENTED_SCHEMA: &str = include_str!("../sql/sqlite/schema.sql");

#[tokio::test]
async fn sqlite_migrations_match_documented_schema() {
    let migration_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sql/migrations/sqlite");
    let migrator = Migrator::new(migration_dir)
        .await
        .expect("SQLite migration directory should be readable");

    let migrated_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("migration database should be available");
    migrator
        .run(&migrated_pool)
        .await
        .expect("SQLite migrations should apply to an empty database");

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
