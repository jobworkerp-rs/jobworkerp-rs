# SQLite migration 手順

このディレクトリの SQLite migration は、アプリケーションに埋め込まれて起動時に
SQLx が自動適用する正規 migration です。ファイルを個別に `sqlite3` で実行しないでください。

## ファイルの役割

- `sqlite/001_base_schema.sql`: 011 の変更直前に相当する初期スキーマ
- `sqlite/002_worker_instance_rdb_status_recovery.sql`: worker instance の復旧用列と索引
- `sqlite/003_add_job_result_search_indexes.sql`: JobResult の検索・集計・一括削除用索引
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
