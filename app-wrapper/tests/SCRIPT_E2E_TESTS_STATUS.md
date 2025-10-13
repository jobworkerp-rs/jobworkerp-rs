# Script Runner E2E テスト実装状況報告

**作成日**: 2025-10-13
**最終更新**: 2025-10-13
**対象**: Script Runner (Python) Phase 2 完了版

---

## エグゼクティブサマリー

Script Runner (Python) のE2Eテストスイートを `WorkflowExecutor` を使った統合実行方式で完全実装し、**全12個のテストケースが合格**しました。Pythonパッケージインストール、セキュリティ検証、タイムアウト処理、並行実行など全ての機能が正常に動作することを確認しました。セキュリティテスト（10個）も全て合格しています。

**テスト結果**:
```bash
cargo test --package app-wrapper --test script_runner_e2e_test -- --ignored --test-threads=1

running 12 tests
test test_concurrent_script_execution ... ok
test test_dangerous_function_rejection ... ok
test test_error_handling_and_reporting ... ok
test test_external_script_https_only ... ok
test test_inline_script_with_base64_arguments ... ok
test test_max_nesting_depth_limit ... ok
test test_nested_json_validation ... ok
test test_python_identifier_validation ... ok
test test_python_package_installation ... ok
test test_script_execution_timeout ... ok
test test_triple_quote_injection_blocked ... ok
test test_use_static_pooling ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 6.46s
```

---

## 📊 実装状況

### ✅ 完了事項

#### 1. E2Eテストスイート作成

**ファイル**: `app-wrapper/tests/script_runner_e2e_test.rs` (892行)

- ✅ 全12個のテストケースを `WorkflowExecutor` を使った統合実行方式で実装
- ✅ ヘルパー関数 `execute_script_workflow()` 実装
- ✅ ワークフローYAML生成関数 `create_script_workflow()` 実装
- ✅ 全テストが合格（12 passed, 0 failed）
- ✅ `#[ignore]` 属性付与（uvとPython、およびバックエンドプロセスが必要）

**実装アプローチ**:
```rust
// WorkflowExecutor を使った統合テスト
let executor = WorkflowExecutor::init(
    app_wrapper_module,
    app,
    http_client,
    Arc::new(workflow),
    Arc::new(input_data),
    None, None, Arc::new(HashMap::new()), None
).await?;

let workflow_stream = executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
// Redisキューを経由してバックエンドworkerで実行
```

**メリット**:
- 実際の本番環境と同じ実行パスを検証
- Redisキュー、worker、protobuf変換など全てのコンポーネントを統合テスト
- 実運用での問題を早期発見可能

#### 2. テストカバレッジ

| # | テスト名 | 検証内容 | 実装状態 | 結果 |
|---|---------|---------|----------|------|
| 1 | `test_inline_script_with_base64_arguments` | Base64エンコード引数の動作 | ✅ 完全実装 | ✅ PASS |
| 2 | `test_triple_quote_injection_blocked` | Triple-quote injection攻撃のブロック | ✅ 完全実装 | ✅ PASS |
| 3 | `test_dangerous_function_rejection` | 危険な関数（eval, os.system）の拒否 | ✅ 完全実装 | ✅ PASS |
| 4 | `test_external_script_https_only` | HTTP/file:// URLの拒否、HTTPS許可 | ✅ 完全実装 | ✅ PASS |
| 5 | `test_python_package_installation` | python.packages メタデータによるインストール | ✅ 完全実装 | ✅ PASS |
| 6 | `test_script_execution_timeout` | タイムアウト設定の動作 | ✅ 完全実装 | ✅ PASS |
| 7 | `test_use_static_pooling` | use_static による仮想環境再利用 | ✅ 完全実装 | ✅ PASS |
| 8 | `test_nested_json_validation` | ネストされたJSON内の危険なパターン検出 | ✅ 完全実装 | ✅ PASS |
| 9 | `test_max_nesting_depth_limit` | 最大ネスト深度（10レベル）制限 | ✅ 完全実装 | ✅ PASS |
| 10 | `test_error_handling_and_reporting` | Python実行エラー（ZeroDivisionError）のハンドリング | ✅ 完全実装 | ✅ PASS |
| 11 | `test_python_identifier_validation` | Python識別子の妥当性検証（有効/無効/予約語） | ✅ 完全実装 | ✅ PASS |
| 12 | `test_concurrent_script_execution` | 5つの並行実行で競合がないこと | ✅ 完全実装 | ✅ PASS |

**合計**: 12テスト / 12テスト合格 (100%)

#### 3. ドキュメント

- ✅ `README_SCRIPT_E2E_TESTS.md` - セットアップ手順、実行方法、トラブルシューティング
- ✅ `SCRIPT_E2E_TESTS_STATUS.md` (本文書) - 実装状況と解決した問題の詳細
- ✅ `protobuf_descriptor_oneof_test.rs` - Protobuf oneof検証テスト

#### 4. セキュリティテスト（別ファイル）

**ファイル**: `app-wrapper/tests/script_security_tests.rs`

```bash
cargo test --package app-wrapper --test script_security_tests -- --test-threads=1

running 10 tests
test performance_tests::test_base64_encoding_performance ... ok
test security_tests::test_base64_prevents_triple_quote_injection ... ok
test security_tests::test_bypass_attempts ... ok
test security_tests::test_dangerous_function_detection ... ok
test security_tests::test_dunder_attribute_detection ... ok
test security_tests::test_max_nesting_depth ... ok
test security_tests::test_nested_object_validation ... ok
test security_tests::test_python_identifier_validation ... ok
test security_tests::test_shell_command_detection ... ok
test security_tests::test_url_schema_validation ... ok

test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

**✅ セキュリティテストは全て合格**

---

## ✅ 解決した問題

### 問題: Protobuf oneof フィールドのJSON変換エラー

**症状**:
- `python.packages` メタデータが正しく設定されているにもかかわらず、worker側で `requirements_spec` が `None` になる
- パッケージインストールがスキップされ、`ModuleNotFoundError: No module named 'requests'` が発生

**根本原因**:
`serde_json::to_value()` がRustのenum variantをJSON化する際に、protobufのJSON表現規則に違反する構造を生成していた:

```json
// serde_jsonの出力（protobuf規則に違反）
{
  "requirements_spec": {
    "Packages": {  // ← Rust enum variant名が含まれている
      "list": ["requests"]
    }
  }
}

// protobufのJSON規則（正しい形式）
{
  "packages": {  // ← フィールド名を直接使用
    "list": ["requests"]
  }
}
```

`ProtobufDescriptor::json_value_to_message()` は正しいprotobuf JSON形式を期待しているため、Rustのenum variant名が含まれていると oneof フィールドを認識できず、結果として `requirements_spec` が欠落していました。

**解決策**:
`app-wrapper/src/workflow/execute/task/run/script.rs` で `PythonCommandRunnerSettings` を `serde_json::to_value()` でJSON化せず、**直接protobufバイナリにエンコード**するように修正:

```rust
// 修正前: JSON経由でエンコード（oneofが失われる）
let settings_value = serde_json::to_value(&settings)?;
let settings_bytes = self.job_executor_wrapper
    .setup_runner_and_settings(&runner, Some(settings_value))
    .await?;

// 修正後: 直接protobufバイナリにエンコード
// serde_jsonはRust enum variantを{"VariantName": {...}}と変換するため、
// protobufのoneof表現と不一致になり、ProtobufDescriptorが認識できない
let settings_bytes = {
    let mut buf = Vec::new();
    prost::Message::encode(&settings, &mut buf)?;
    buf
};
```

**検証**:
1. `app-wrapper/tests/protobuf_descriptor_oneof_test.rs` を追加し、`ProtobufDescriptor::json_value_to_message()` がoneofフィールドを正しく処理できないことを実証
2. 修正後、worker側のログで以下を確認:
   ```
   runner_settings: [10, 4, 51, 46, 49, 50, 26, 10, 10, 8, 114, 101, 113, 117, 101, 115, 116, 115]
   ```
   - `[10, 4, 51, 46, 49, 50]` = `python_version="3.12"` ✅
   - `[26, 10, 10, 8, 114, 101, 113, 117, 101, 115, 116, 115]` = `requirements_spec.packages=["requests"]` ✅
3. test_python_package_installation が合格し、requestsパッケージが正常にインストール・使用されることを確認

**同様の修正**:
`PythonCommandArgs` も同じ理由で直接protobufバイナリにエンコードしています（script.rs:543-553）。

---

## 🔍 技術的詳細

### ワークフロー実行フロー（統合テスト）

```
execute_script_workflow()
  ↓
WorkflowLoader::load_workflow()
  ↓
WorkflowExecutor::init()
  ↓
WorkflowExecutor::execute_workflow()
  ↓
TaskExecutor::execute()
  ↓
ScriptTaskExecutor::execute()
  ↓
ScriptTaskExecutor::to_python_command_args() // script_code 生成
  ↓
ScriptTaskExecutor::to_python_runner_settings() // settings 生成
  ↓
prost::Message::encode(&settings) // 直接protobufエンコード ✅
  ↓
enqueue_with_worker_or_temp() // Redisキューにジョブ投入
  ↓
[Redis Queue]
  ↓
[Backend Worker Process]
  ↓
PythonCommandRunner::load(settings_bytes) // worker側でデコード
  ↓
uv venv create // 仮想環境作成
  ↓
uv pip install requests // requirements_specに基づきパッケージインストール ✅
  ↓
PythonCommandRunner::run(args_bytes) // スクリプト実行
  ↓
✅ Success
```

### Protobuf Oneof フィールドの正しい扱い方

**教訓**: Protobuf の oneof フィールドを含む構造体を扱う際は、以下のいずれかの方法を使用する:

1. **推奨**: `prost::Message::encode()` で直接protobufバイナリにエンコード
2. **非推奨**: `serde_json::to_value()` → `ProtobufDescriptor::json_value_to_message()`
   - Rustのenum variant名がJSONに含まれてしまい、protobuf JSON規則に違反する

**適用箇所**:
- `PythonCommandRunnerSettings` (oneof `requirements_spec`)
- `PythonCommandArgs` (oneof `script`, oneof `input_data`)

---

## 📋 完了したステップ

### Phase 1: 問題調査
1. ✅ E2Eテストの実行エラーを確認
2. ✅ バックエンドログから `requirements_spec` が欠落していることを発見
3. ✅ metadata伝播チェーン全体をトレース
4. ✅ `serde_json::to_value()` と `ProtobufDescriptor::json_value_to_message()` の問題を特定

### Phase 2: 問題検証
5. ✅ `protobuf_descriptor_oneof_test.rs` を作成し、`ProtobufDescriptor` がoneof処理できないことを実証
6. ✅ Protobuf JSON規則とRust serde_jsonの違いを理解

### Phase 3: 修正実装
7. ✅ `script.rs` で `PythonCommandRunnerSettings` を直接protobufエンコードに変更
8. ✅ `PythonCommandArgs` も同様に修正（既に実装済み）

### Phase 4: 検証
9. ✅ test_python_package_installation 実行確認（合格）
10. ✅ 全12個のE2Eテスト実行確認（全て合格）
11. ✅ ドキュメント更新

---

## 📦 成果物

### 新規作成ファイル

1. **`app-wrapper/tests/script_runner_e2e_test.rs`** (892行)
   - 全12個のE2Eテストケース（全て合格）
   - ヘルパー関数（`execute_script_workflow`, `create_script_workflow`）
   - テスト用AppWrapperModule初期化関数

2. **`app-wrapper/tests/README_SCRIPT_E2E_TESTS.md`** (320行)
   - セットアップ手順
   - テスト実行方法
   - テストカバレッジ一覧
   - トラブルシューティング
   - テスト開発ガイド

3. **`app-wrapper/tests/SCRIPT_E2E_TESTS_STATUS.md`** (本文書)
   - 実装状況の詳細報告
   - 解決した問題の技術的詳細
   - Protobuf oneof フィールドの正しい扱い方

4. **`app-wrapper/tests/protobuf_descriptor_oneof_test.rs`** (148行)
   - Protobuf oneof フィールドのJSON変換問題を検証するテスト
   - `ProtobufDescriptor::json_value_to_message()` の制限を実証

### 更新ファイル

1. **`app-wrapper/src/workflow/execute/task/run/script.rs`**
   - `PythonCommandRunnerSettings` を直接protobufエンコードに変更（525-539行）
   - 詳細なコメントで理由を説明

2. **`app-wrapper/tests/script_security_tests.rs`**
   - E2Eテストへの参照を追加

---

## 🎯 結論

Script Runner (Python) のE2Eテストスイートは**完全に実装完了し、全12テストが合格**しました。

### 主要な成果

1. **統合テスト環境の構築**: Redisキュー + worker architectureでの実際の実行フローを検証
2. **Protobuf oneof問題の解決**: `serde_json::to_value()` とprotobuf JSON規則の不一致を発見・修正
3. **完全なテストカバレッジ**: セキュリティ、パフォーマンス、エラーハンドリングを網羅
4. **技術的知見の蓄積**: Protobuf oneofフィールドの正しい扱い方をドキュメント化

### 技術的学び

**Protobuf oneof フィールドを含む構造体の正しい処理方法**:
- ✅ `prost::Message::encode()` で直接バイナリエンコード
- ❌ `serde_json::to_value()` → JSON経由（Rust enum variant名が含まれ、protobuf規則に違反）

この知見は他のrunner実装（GRPC_UNARY, DOCKER等）でも適用可能です。

---

## 📚 参考資料

- **実装計画書**: `github/docs/runners/script-runner-python-implementation-plan.md`
- **セキュリティガイド**: `github/docs/workflow/script-process-security.md`
- **Script実装**: `github/app-wrapper/src/workflow/execute/task/run/script.rs`
- **セキュリティテスト**: `github/app-wrapper/tests/script_security_tests.rs`
- **E2Eテスト**: `github/app-wrapper/tests/script_runner_e2e_test.rs`
- **E2Eテストガイド**: `github/app-wrapper/tests/README_SCRIPT_E2E_TESTS.md`
- **Protobuf Oneof検証テスト**: `github/app-wrapper/tests/protobuf_descriptor_oneof_test.rs`

---

**報告者**: Claude Code
**ステータス**: ✅ 完了
**テスト結果**: 12/12 合格 (100%)
