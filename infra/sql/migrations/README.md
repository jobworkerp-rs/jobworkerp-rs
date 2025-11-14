# 既存環境用DBマイグレーションファイル

このディレクトリには、既存環境（運用中のDB）に対して手動実行するマイグレーションファイルを保管しています。

## ⚠️ 重要な注意事項

**このディレクトリのファイルはsqlxの自動マイグレーション対象外です。**

- 新規環境では`infra/sql/sqlite/002_schema.sql`または`infra/sql/mysql/002_worker.sql`が自動適用される
- これらのファイルは既存環境に対して**手動で実行**する必要がある

## ディレクトリ構成

```
infra/sql/
├── sqlite/
│   ├── 001_init.sql              # sqlx自動マイグレーション対象
│   └── 002_schema.sql            # sqlx自動マイグレーション対象（created_at含む）
├── mysql/
│   ├── 001_init_mysql.sql        # sqlx自動マイグレーション対象
│   └── 002_worker.sql            # sqlx自動マイグレーション対象（created_at含む）
└── migrations/                    # sqlx自動マイグレーション対象外
    ├── sqlite/
    │   ├── 003_add_created_at_columns.sql
    │   ├── rollback_003_add_created_at_columns.sql
    │   ├── 004_job_processing_status.sql
    │   ├── rollback_004_job_processing_status.sql
    │   ├── 005_add_job_result_indexes.sql
    │   └── rollback_005_add_job_result_indexes.sql
    └── mysql/
        ├── 003_add_created_at_columns.sql
        ├── rollback_003_add_created_at_columns.sql
        ├── 004_job_processing_status.sql
        ├── rollback_004_job_processing_status.sql
        ├── 005_add_job_result_indexes.sql
        └── rollback_005_add_job_result_indexes.sql
```

## マイグレーション戦略

### 新規環境（開発環境・新規デプロイ）

**自動適用される**:
- SQLite: `002_schema.sql`に`created_at`カラムとインデックスが含まれている
- MySQL: `002_worker.sql`に`created_at`カラムとインデックスが含まれている

### 既存環境（運用中のDB）

**手動実行が必要**:
- `migrations/`ディレクトリ内のファイルを手動で実行

## マイグレーション実行手順

### MySQL本番環境

```bash
# 1. バックアップ取得
mysqldump -u root -p jobworkerp > backup_$(date +%Y%m%d_%H%M%S).sql

# 2. マイグレーション実行
mysql -u root -p jobworkerp < infra/sql/migrations/mysql/003_add_created_at_columns.sql

# 3. 確認
mysql -u root -p jobworkerp -e "DESCRIBE runner;" | grep created_at
mysql -u root -p jobworkerp -e "DESCRIBE worker;" | grep created_at
mysql -u root -p jobworkerp -e "SHOW INDEX FROM runner WHERE Key_name='idx_runner_created_at';"
mysql -u root -p jobworkerp -e "SHOW INDEX FROM worker WHERE Key_name='idx_worker_created_at';"

# ロールバック（問題発生時）
mysql -u root -p jobworkerp < infra/sql/migrations/mysql/rollback_003_add_created_at_columns.sql
```

### SQLite（既存データ保持が必要な場合）

```bash
# 1. バックアップ取得
cp data/jobworkerp.db data/jobworkerp_backup_$(date +%Y%m%d_%H%M%S).db

# 2. マイグレーション実行
sqlite3 data/jobworkerp.db < infra/sql/migrations/sqlite/003_add_created_at_columns.sql

# 3. 確認
sqlite3 data/jobworkerp.db "PRAGMA table_info(runner);" | grep created_at
sqlite3 data/jobworkerp.db "PRAGMA table_info(worker);" | grep created_at
sqlite3 data/jobworkerp.db "PRAGMA index_list(runner);" | grep idx_runner_created_at
sqlite3 data/jobworkerp.db "PRAGMA index_list(worker);" | grep idx_worker_created_at

# ロールバック（問題発生時）
sqlite3 data/jobworkerp.db < infra/sql/migrations/sqlite/rollback_003_add_created_at_columns.sql
```

## ファイル一覧

### SQLite

- `sqlite/003_add_created_at_columns.sql`: `created_at`カラムとインデックス追加
- `sqlite/rollback_003_add_created_at_columns.sql`: ロールバックスクリプト
- `sqlite/004_job_processing_status.sql`: JobProcessingStatusテーブル作成（Sprint 3）
- `sqlite/rollback_004_job_processing_status.sql`: ロールバックスクリプト
- `sqlite/005_add_job_result_indexes.sql`: JobResult検索インデックス追加（Sprint 4）
- `sqlite/rollback_005_add_job_result_indexes.sql`: ロールバックスクリプト

### MySQL

- `mysql/003_add_created_at_columns.sql`: `created_at`カラムとインデックス追加
- `mysql/rollback_003_add_created_at_columns.sql`: ロールバックスクリプト
- `mysql/004_job_processing_status.sql`: JobProcessingStatusテーブル作成（Sprint 3）
- `mysql/rollback_004_job_processing_status.sql`: ロールバックスクリプト

## マイグレーション内容（003）

### 追加されるカラム

- `runner.created_at`: BIGINT NOT NULL DEFAULT 0 (レコード作成時刻、ミリ秒)
- `worker.created_at`: BIGINT NOT NULL DEFAULT 0 (レコード作成時刻、ミリ秒)

### ⚠️ created_atカラムの注意事項

#### 既存レコードのcreated_at値
- **新規レコード**: アプリケーション層で`chrono::Utc::now().timestamp_millis()`を設定
- **マイグレーション時の既存レコード**: マイグレーション実行時の現在時刻で一括設定
- **マイグレーション実行後に作成されるレコード**: 正確な作成時刻が設定される

#### 管理画面での影響
- created_atソート時、マイグレーション実行時刻が基準となる
- マイグレーション前の古いレコードの正確な作成時刻は不明
- UI表示推奨: `created_at`をそのまま表示（マイグレーション実行時刻として表示される）

#### マイグレーションSQL実行内容
マイグレーションSQLで以下が実行されます:
```sql
-- MySQL
UPDATE runner SET created_at = UNIX_TIMESTAMP() * 1000 WHERE created_at = 0;
UPDATE worker SET created_at = UNIX_TIMESTAMP() * 1000 WHERE created_at = 0;

-- SQLite
UPDATE runner SET created_at = CAST(strftime('%s', 'now') AS INTEGER) * 1000 WHERE created_at = 0;
UPDATE worker SET created_at = CAST(strftime('%s', 'now') AS INTEGER) * 1000 WHERE created_at = 0;
```

### 追加されるインデックス

**Runnerテーブル**:
- `idx_runner_type`: 種別フィルタ用
- `idx_runner_created_at`: 作成日時ソート用

**Workerテーブル**:
- `idx_worker_runner_id`: Runner別Worker検索用
- `idx_worker_channel`: チャネル別検索用
- `idx_worker_periodic_interval`: 定期実行フィルタ用
- `idx_worker_created_at`: 作成日時ソート用

## マイグレーション内容（004）

### 追加されるテーブル

**job_processing_status**: Job実行状態RDBインデックステーブル

- `job_id`: BIGINT PRIMARY KEY
- `status`: INT NOT NULL (PENDING=1, RUNNING=2, WAIT_RESULT=3, CANCELLING=4)
- `worker_id`: BIGINT NOT NULL
- `channel`: TEXT/VARCHAR(255) NOT NULL
- `priority`: INT NOT NULL
- `enqueue_time`: BIGINT NOT NULL
- `pending_time`: BIGINT (PENDING状態開始時刻)
- `start_time`: BIGINT (RUNNING状態開始時刻)
- `is_streamable`: BOOLEAN NOT NULL DEFAULT 0
- `broadcast_results`: BOOLEAN NOT NULL DEFAULT 0
- `version`: BIGINT NOT NULL (楽観的ロック用)
- `deleted_at`: BIGINT (論理削除時刻)
- `updated_at`: BIGINT NOT NULL

### 追加されるインデックス（5個）

- `idx_jps_status_active`: status別検索（deleted_at IS NULL条件付き）
- `idx_jps_worker_id_active`: Worker別検索
- `idx_jps_channel_active`: チャネル別検索
- `idx_jps_start_time_active`: 実行開始時刻ソート
- `idx_jps_status_start`: status + start_time複合インデックス

### 有効化条件

- デフォルト: **無効** (`JOB_STATUS_RDB_INDEXING=false`)
- 大規模環境のみ有効化推奨（100万件以上のJob滞留）

## マイグレーション内容（005）

### 追加されるインデックス（5個）

**job_result**: JobResult検索インデックス（Sprint 4 - JobResultService拡張）

- `idx_job_result_status`: ステータスフィルタ用
  - 用途: FindListBy/CountByのstatus条件
- `idx_job_result_start_time`: 開始時刻範囲検索用
  - 用途: FindListBy/CountByのstart_time_from/to条件
- `idx_job_result_end_time`: 終了時刻範囲検索用
  - 用途: FindListBy/CountByのend_time_from/to条件、DeleteBulkのend_time_before条件
- `idx_job_result_end_status`: 終了時刻+ステータス複合インデックス
  - 用途: DeleteBulk with end_time_before AND status（例: 古い成功結果のみ削除）
  - 理由: 一括削除の効率化（時刻とステータスの両方で絞り込み）
- `idx_job_result_worker_end`: Worker ID+終了時刻降順複合インデックス
  - 用途: FindListByでworker_id条件 + end_time DESCソート（最新順）
  - 理由: Worker別クエリでの時刻ソート効率化

### インデックスの効果

- **FindListBy/CountBy**: フィルタリング性能向上（100万件データでも1秒以内）
- **DeleteBulk**: 一括削除性能向上（古いデータのみ効率的に削除）
- **Worker別検索**: Worker別のJobResult検索が高速化

### マイグレーション実行例

#### MySQL
```bash
# バックアップ取得
mysqldump -u root -p jobworkerp > backup_$(date +%Y%m%d_%H%M%S).sql

# マイグレーション実行
mysql -u root -p jobworkerp < infra/sql/migrations/mysql/005_add_job_result_indexes.sql

# 確認
mysql -u root -p jobworkerp -e "SHOW INDEX FROM job_result;"

# ロールバック（問題発生時）
mysql -u root -p jobworkerp < infra/sql/migrations/mysql/rollback_005_add_job_result_indexes.sql
```

#### SQLite
```bash
# バックアップ取得
cp data/jobworkerp.db data/jobworkerp_backup_$(date +%Y%m%d_%H%M%S).db

# マイグレーション実行
sqlite3 data/jobworkerp.db < infra/sql/migrations/sqlite/005_add_job_result_indexes.sql

# 確認
sqlite3 data/jobworkerp.db "PRAGMA index_list(job_result);"

# ロールバック（問題発生時）
sqlite3 data/jobworkerp.db < infra/sql/migrations/sqlite/rollback_005_add_job_result_indexes.sql
```

## 注意事項

1. **本番環境では必ずバックアップを取得してから実行**
2. **メンテナンス時間帯に実行を推奨**（深夜2:00-4:00等）
3. **ロールバックスクリプトも用意されている**
4. **新規環境ではこれらのファイルは不要**（002スキーマに含まれている）
5. **このディレクトリのファイルはsqlxの自動マイグレーション対象外**
6. **004_job_processing_status.sqlはデフォルト無効機能のため、実行は任意**
7. **005_add_job_result_indexes.sqlはJobResultService拡張機能のため、管理画面実装後に実行**
