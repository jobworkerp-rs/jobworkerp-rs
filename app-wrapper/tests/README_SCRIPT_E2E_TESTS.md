# Script Runner E2E Tests

## 概要

`script_runner_e2e_test.rs` は、Script Runner (Python) 実装のエンドツーエンドテストスイートです。

## 前提条件

### 必須

1. **uv のインストール**
   ```bash
   # macOS/Linux
   curl -LsSf https://astral.sh/uv/install.sh | sh

   # Windows
   powershell -c "irm https://astral.sh/uv/install.ps1 | iex"
   ```

2. **uv が PATH に含まれていることを確認**
   ```bash
   uv --version
   ```

3. **Python 3.8-3.13**
   - uv が自動的に必要なバージョンをダウンロード・管理

## テスト実行方法

### 全テスト実行

```bash
cd github
cargo test --package app-wrapper --test script_runner_e2e_test -- --ignored --test-threads=1 --nocapture
```

### 特定のテスト実行

```bash
# Test 1: Base64エンコード付き引数
cargo test --package app-wrapper test_inline_script_with_base64_arguments -- --ignored --nocapture

# Test 2: Triple-quote injection攻撃
cargo test --package app-wrapper test_triple_quote_injection_blocked -- --ignored --nocapture

# Test 3: 危険な関数の検出
cargo test --package app-wrapper test_dangerous_function_rejection -- --ignored --nocapture
```

## テストカバレッジ

| テスト名 | 検証内容 | 実装状態 |
|---------|---------|----------|
| `test_inline_script_with_base64_arguments` | Base64エンコード引数の動作 | ✅ 実装済み |
| `test_triple_quote_injection_blocked` | Triple-quote injection攻撃のブロック | ✅ 実装済み |
| `test_dangerous_function_rejection` | 危険な関数の引数検証 | ✅ 実装済み |
| `test_external_script_https_only` | 外部スクリプトHTTPS検証 | ✅ 実装済み |
| `test_python_package_installation` | Pythonパッケージインストール | ✅ 実装済み |
| `test_script_execution_timeout` | スクリプト実行タイムアウト | ✅ 実装済み |
| `test_use_static_pooling` | use_static プーリング動作 | ✅ 実装済み |
| `test_nested_json_validation` | ネストされたJSON検証 | ✅ 実装済み |
| `test_max_nesting_depth_limit` | 最大ネスト深度制限 | ✅ 実装済み |
| `test_error_handling_and_reporting` | エラーハンドリング | ✅ 実装済み |
| `test_python_identifier_validation` | Python識別子検証 | ✅ 実装済み |
| `test_concurrent_script_execution` | 並行実行 | ✅ 実装済み |

## テスト実装状況

### ✅ 完全実装済み

全12個のテストが `WorkflowExecutor` を使った直接実行方式で実装されています。

**実装アプローチ**:
- `WorkflowExecutor::init()` でワークフローを初期化
- `execute_workflow()` でストリーム実行
- 結果を直接取得して検証

**メリット**:
- リモートジョブワーカーへの依存なし
- 同期的な結果取得が可能
- テストの信頼性が向上

### ✅ 実装済みテスト一覧

#### Test 1: `test_inline_script_with_base64_arguments`

**検証内容**:
- インラインPythonスクリプトの実行
- Base64エンコードされた引数の注入
- 変数名での直接アクセス

**期待動作**:
```python
# 引数として message="Hello from Python!", count=42 を渡す
# 以下のように直接アクセス可能
print(json.dumps({
    "message": message,      # "Hello from Python!"
    "count": count,          # 42
    "doubled": count * 2     # 84
}))
```

**実行例**:
```bash
# 注意: 現在はデータベース接続でタイムアウトする可能性があります
cargo test --package app-wrapper test_inline_script_with_base64_arguments -- --ignored --nocapture
```

**期待出力** (データベース接続成功時):
```
✅ Worker created: test-script-base64-worker (ID: 123456)
✅ Workflow executed successfully
✅ Test 1 execution completed
   - Output: {
       "message": "Hello from Python!",
       "count": 42,
       "doubled": 84
     }
✅ Test 1: Base64 encoding with arguments succeeded
```

**実装内容**:
- WorkflowExecutorを使った完全な実行フロー
- 出力値の検証 (message, count, doubled)
- アサーションによる期待値チェック

---

#### Test 2: `test_triple_quote_injection_blocked`

**検証内容**:
- Triple-quote エスケープ攻撃のブロック
- 悪意あるペイロードが文字列データとして安全に扱われる

**攻撃ペイロード**:
```python
'''
import os
os.system('echo INJECTION_SUCCESSFUL')
'''
```

**期待動作**:
- ペイロードは実行されず、文字列として受け取られる
- `INJECTION_SUCCESSFUL` は出力に含まれない
- Base64エンコードにより安全に処理される

**実装内容**:
- 悪意あるペイロードを含む引数でスクリプト実行
- 出力に `INJECTION_SUCCESSFUL` が含まれないことを確認
- `execution_safe: true` が返されることを検証

---

#### Test 3-12: 完全実装

以下のテストも完全に実装されています:

- **Test 3**: 危険な関数（eval, os.system）を含む引数の拒否を検証
- **Test 4**: HTTP/file:// URLの拒否、HTTPS URLのみ許可を検証
- **Test 5**: python.packages メタデータによるパッケージインストールを検証
- **Test 6**: タイムアウト設定が正しく動作することを検証
- **Test 7**: use_static による仮想環境再利用を検証
- **Test 8**: ネストされたJSON内の危険なパターン検出を検証
- **Test 9**: 最大ネスト深度（10レベル）制限を検証
- **Test 10**: Python実行エラー（ZeroDivisionError）のハンドリングを検証
- **Test 11**: Python識別子の妥当性検証（有効/無効/予約語）
- **Test 12**: 5つの並行実行で競合がないことを検証

## トラブルシューティング

### uv が見つからない

```bash
# PATH を確認
echo $PATH

# uv の場所を確認
which uv

# 再インストール
curl -LsSf https://astral.sh/uv/install.sh | sh
source ~/.bashrc  # または ~/.zshrc
```

### テストがタイムアウトする

デフォルトタイムアウトは120秒です。必要に応じて調整してください:

```rust
// script_runner_e2e_test.rs の create_and_execute_script_workflow() 内
let output = job_executor
    .enqueue_with_worker_name_and_output_json(
        Arc::new(HashMap::new()),
        worker_name,
        &input_data,
        None,
        300, // 5分に延長
        false,
    )
    .await?;
```

### データベース接続エラー

`create_hybrid_test_app()` でデータベース接続エラーが発生する場合:

```bash
# 環境変数を確認
echo $DATABASE_URL
echo $REDIS_URL

# 必要に応じて設定
export DATABASE_URL="sqlite::memory:"
export REDIS_URL="redis://localhost:6379"
```

必ず `--test-threads=1` を指定してください（データベース共有のため）:

```bash
cargo test --package app-wrapper --test script_runner_e2e_test -- --ignored --test-threads=1
```

### Python バージョンエラー

特定のPythonバージョンが必要な場合:

```yaml
metadata:
  python.version: "3.11"  # または "3.12", "3.10" など
```

## テスト開発ガイド

### 新しいテストの追加

1. **ワークフロー定義を作成**
   ```rust
   let workflow = create_script_workflow(
       "test-name",
       "Python script code",
       json!({"arg1": "value1"}),
       Some(json!({"python.version": "3.12"})),
   );
   ```

2. **実行と検証**
   ```rust
   let result = create_and_execute_script_workflow(
       app.clone(),
       "test-worker-name",
       workflow,
       json!({}),
   ).await?;

   assert_eq!(result["expected_key"], "expected_value");
   ```

3. **#[ignore] 属性を付与**
   ```rust
   #[tokio::test]
   #[ignore = "Requires uv and Python installation"]
   async fn test_new_feature() -> Result<()> { ... }
   ```

### ヘルパー関数

#### `create_script_workflow()`

スクリプトタスクを含むワークフロー定義を生成します。

**引数**:
- `name: &str` - ワークフロー名
- `script_code: &str` - Pythonスクリプトコード
- `arguments: serde_json::Value` - スクリプト引数
- `metadata: Option<serde_json::Value>` - タスクメタデータ

**例**:
```rust
let workflow = create_script_workflow(
    "example-test",
    r#"
import json
print(json.dumps({"result": value * 2}))
    "#,
    json!({"value": 21}),
    Some(json!({"python.packages": "requests"})),
);
```

#### `create_and_execute_script_workflow()`

ワークフローを作成して実行し、結果を返します。

**引数**:
- `app: Arc<AppModule>` - アプリケーションモジュール
- `worker_name: &str` - ワーカー名
- `workflow: serde_json::Value` - ワークフロー定義
- `input_data: serde_json::Value` - ワークフロー入力データ

**戻り値**:
- `Result<serde_json::Value>` - スクリプトの出力 (JSON)

## 参考資料

- **実装計画書**: `github/docs/runners/script-runner-python-implementation-plan.md`
- **セキュリティガイド**: `github/docs/workflow/script-process-security.md`
- **スクリプト実装**: `github/app-wrapper/src/workflow/execute/task/run/script.rs`
- **セキュリティテスト**: `github/app-wrapper/tests/script_security_tests.rs`

## 次のステップ

1. **Test 3-12 の完全実装**
   - プレースホルダーを実際の実行コードに置き換え
   - 各テストの期待動作を検証

2. **パフォーマンステスト**
   - 初回実行 vs 2回目以降 (`use_static=true`)
   - 並行実行時のスループット測定

3. **エラーケーステスト**
   - 無効なPythonバージョン
   - 存在しないパッケージ
   - ネットワークエラー

4. **統合テストの拡充**
   - 複数タスクを含むワークフロー
   - 条件分岐とループ
   - エラーハンドリングとリトライ

---

**作成日**: 2025-10-13
**最終更新**: 2025-10-13
**対象バージョン**: Phase 2完了版
