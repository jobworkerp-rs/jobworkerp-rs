# RDB migration 手順

## MySQL

MySQL の migration はアプリケーション起動時には自動適用されず、管理者が
手動で適用します。`infra/sql/mysql/schema.sql` は全 migration 適用後の完成形を
確認する参照用スキーマで、新規データベースを初期化する場合に使用できます。

`mysql/001_base_schema.sql` は新規データベース用の破壊的な初期化スキーマです。
既存データベースには適用しないでください。復旧用スキーマ追加前の既存 MySQL は
このベーススキーマと同等の状態にあることを前提に、次の手順で 002 以降を番号順に適用します。

1. データベースのバックアップを取得し、停止中のアプリケーションから接続がないことを確認します。
2. `mysql/` の未適用 migration を番号順に確認します。
3. `mysql -h HOST -u USER -p DATABASE < infra/sql/migrations/mysql/002_worker_instance_rdb_status_recovery.sql` のように各ファイルを適用します。
4. `SHOW COLUMNS FROM job_processing_status` と `SHOW INDEX FROM job_processing_status` で適用結果を確認します。
5. アプリケーションを起動し、ジョブ復旧と通常のジョブ処理を確認します。

新規データベースでは、Docker Compose の初期化設定または `schema.sql` を使用して
完成形を作成できます。既存データベースに `schema.sql` や `001_base_schema.sql`
を適用するとデータを失うため、必ず番号付き差分だけを適用してください。

新しい MySQL migration を追加する場合は、現在の最大番号の次に連番を割り当て、
完成形の `schema.sql` とスナップショット一致テストも更新します。

### `SLACK_POST_MESSAGE` runner の削除

このリリースでは、MySQL の `runner` 表から `id = 5`、`name = 'SLACK_POST_MESSAGE'`、
`type = 5` がすべて一致する組み込み runner 定義を削除します。MySQL は自動移行しないため、
アプリケーションを停止し、バックアップを取得した後、次の差分を手動適用してください。

```bash
mysql -h HOST -u USER -p DATABASE < infra/sql/migrations/mysql/004_remove_slack_post_message_runner.sql
```

この差分は一致する runner 定義だけを削除し、runner の ID が異なる行、名前または種別が異なる行、
worker/job の記録は保持します。既存の Slack worker とその未処理 job は記録として残りますが、
このリリースでは実行できません。更新前に対象 job を完了またはキャンセルし、worker を停止して、
必要な処理を別の runner に移行してください。

新規データベースでは `infra/sql/mysql/schema.sql` が削除後の完成形です。既存データベースには上記の番号付き
差分だけを適用してください。

## SQLite

このディレクトリの SQLite migration は、アプリケーションに埋め込まれて起動時に
SQLx が自動適用する正規 migration です。ファイルを個別に `sqlite3` で実行しないでください。

## ファイルの役割

- `sqlite/001_base_schema.sql`: 011 の変更直前に相当する初期スキーマ
- `sqlite/002_worker_instance_rdb_status_recovery.sql`: worker instance の復旧用列と索引
- `sqlite/003_add_job_result_search_indexes.sql`: JobResult の検索・集計・一括削除用索引
- `sqlite/004_register_sandbox_runner.sql`: SANDBOX runner 定義の登録
- `sqlite/005_remove_slack_post_message_runner.sql`: 対象条件に一致する Slack runner 定義の削除
- `../sqlite/schema.sql`: 全 migration 適用後の完成形を確認する参照用スキーマ
- `mysql/`: MySQL 用の既存 migration（SQLite の起動時 migration とは別系統）

`schema.sql` は実行用スキーマでも migration の正本でもありません。SQLite の変更は
連番の migration として追加し、既存ファイルの内容を変更しないでください。

## 既存 SQLite DB の更新手順

SQLite は単一プロセス運用を前提とします。更新中に別の worker または frontend を起動しないでください。

この自動移行は、011 より前のすべての SQLite migration が適用済みである DB を対象とします。
それ以前の migration が未適用の DB はサポート対象外であり、起動前に対応する旧 migration を完了してください。

1. ジョブの受付を停止し、実行中ジョブが完了またはキャンセルされるまで待ちます。
2. worker、frontend など JobWorkerP の全プロセスを停止します。
3. DB と WAL 関連ファイルを同じ時点でバックアップします。

   ```bash
   cp data/jobworkerp.db data/jobworkerp.db.backup
   test ! -e data/jobworkerp.db-wal || cp data/jobworkerp.db-wal data/jobworkerp.db-wal.backup
   test ! -e data/jobworkerp.db-shm || cp data/jobworkerp.db-shm data/jobworkerp.db-shm.backup
   ```

4. 新しいバイナリを起動します。SQLite の正規 migration が自動適用され、適用履歴は
   `_sqlx_migrations` に記録されます。migration の途中でエラーになった場合はプロセスを停止し、
   ログのエラーを確認してからバックアップを復元してください。
5. 起動後、適用履歴と主要な構造を確認します。

   ```bash
   sqlite3 data/jobworkerp.db \
     "SELECT version, success FROM _sqlx_migrations ORDER BY version;"
   sqlite3 data/jobworkerp.db \
     "PRAGMA table_info(job_processing_status);"
   sqlite3 data/jobworkerp.db \
     "PRAGMA index_list(job_processing_status);"
   ```

6. ジョブの受付を再開し、worker を起動します。

`SLACK_POST_MESSAGE` を使っていた worker 定義と job の行は SQLite migration では削除されませんが、
このリリースではその worker/job を実行できません。更新前に対象 job を完了またはキャンセルし、
worker を停止して、必要な処理を別の runner に移行してください。

### migration に失敗した場合

自動修復や `schema.sql` の手動適用は行わないでください。プロセスを停止した状態で
DB と WAL 関連ファイルを退避し、原因と migration の適用履歴を記録してから、バックアップへ
復元するか管理者に相談してください。不完全な DB を無理に次の migration へ進めないでください。

## 新しい migration の追加

1. 現在の migration の最大番号の次に連番を割り当てます。
2. SQLite で実行できる SQL を追加し、既存データを保持する変更には必要な境界条件を確認します。
3. `schema.sql` を全 migration 適用後の構造へ更新します。
4. migration 適用結果と `schema.sql` の一致テストを実行します。

```bash
cargo test -p infra --test sqlite_schema_test -- --test-threads=1
```

migration の履歴を持たない既存 DB は、構造が不完全な場合に自動修復されません。
更新前バックアップを取得し、migration の適用結果と DB の状態を確認してください。
