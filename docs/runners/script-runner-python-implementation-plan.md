# Script Runner (Python) 実装計画書

## 文書管理

- **作成日**: 2025-10-13
- **バージョン**: 2.1.0 (Phase 2着手前・セキュリティ緊急パッチ計画追加)
- **ステータス**: Phase 1完了・Phase 2セキュリティ緊急対応計画追加
- **最終更新**: 2025-10-13
- **Phase 1完了日**: 2025-10-13
- **セキュリティレビュー完了**: 2025-10-13

## 目次

1. [概要](#概要)
2. [Phase 1実装完了サマリー](#phase-1実装完了サマリー)
3. [Serverless Workflow仕様との整合性](#serverless-workflow仕様との整合性)
4. [Phase 1: 実装済み機能](#phase-1-実装済み機能)
5. [セキュリティレビューと緊急パッチ計画](#セキュリティレビューと緊急パッチ計画)
6. [Phase 2: セキュリティ強化と高度な機能](#phase-2-セキュリティ強化と高度な機能)
7. [Phase 3: JavaScript サポート](#phase-3-javascript-サポート)
8. [Phase 4: 高度な機能](#phase-4-高度な機能)
9. [使用方法](#使用方法)
10. [トラブルシューティング](#トラブルシューティング)
11. [変更履歴](#変更履歴)

---

## 概要

jobworkerp-rsプロジェクトにServerless Workflow DSL v1.0.0準拠の**Script Process (Python)**機能を実装しました。

### 目標

- ✅ Serverless Workflow仕様の`run.script`タスクをPythonでサポート
- ✅ 既存の`PYTHON_COMMAND` runnerを拡張・活用
- ✅ workflowスキーマへの統合とシームレスな実行
- ✅ 簡易なデータ変換処理をRustバイナリビルドなしで実行可能に

### Phase 1完了時点の対象範囲

- ✅ Python Script Processの実装
- ✅ インラインコード実行
- ✅ 外部スクリプトURL参照（基本実装）
- ✅ 環境変数サポート
- ✅ 入力/出力データハンドリング
- ✅ ワークフロースキーマへの統合
- ✅ ランタイム式評価（jq/liquid）
- ✅ 基本的なセキュリティ検証（Python識別子、予約語チェック）
- ❌ JavaScript実装（Phase 3で対応）
- ❌ 高度なセキュリティ機能（Phase 2で対応）

---

## Phase 1実装完了サマリー

### 完了事項 (2025-10-13)

#### 1. スキーマ定義とRust型生成

**完了ファイル**:
- `runner/schema/workflow.yaml` - runScript定義追加
- `runner/schema/workflow.json` - JSON Schema生成完了
- `app-wrapper/src/workflow/definition/workflow.rs` - typify型生成完了 (8298行)

**追加された型**:
```rust
pub enum RunTaskConfiguration {
    Worker(RunWorker),
    Runner(RunRunner),
    Function(RunFunction),
    Script(RunScript),  // ✅ 新規追加
}

pub struct RunScript {
    pub script: ScriptConfiguration,
}

// ScriptConfiguration はtypifyにより自動生成
// Variant0 (インラインコード) とVariant1 (外部ソース) を持つenum
```

#### 2. コアロジック実装

**完了ファイル**:
- `app-wrapper/src/workflow/execute/task/run/script.rs` (490行)
  - `ScriptTaskExecutor` 実装完了
  - Python実行ロジック完成
  - エラーハンドリング実装済み

- `app-wrapper/src/workflow/execute/task/run.rs`
  - `RunTaskConfiguration::Script`分岐追加
  - OpenTelemetry metadata injection実装

- `app-wrapper/src/workflow/definition/workflow/supplement.rs` (+100行)
  - `ValidatedLanguage` enum実装
  - `PythonScriptSettings` 実装
  - メタデータパーシングロジック

#### 3. 実装済み機能詳細

| 機能 | 状態 | 備考 |
|------|------|------|
| インラインコード実行 | ✅ | `script.code`フィールド対応 |
| 外部スクリプトURL | ✅ | `script.source`フィールド対応 |
| 引数注入 | ✅ | `arguments`の各キーがPython変数化 |
| 環境変数 | ✅ | `environment`フィールド対応 |
| ランタイム式評価 | ✅ | jq/liquid式を評価後に注入 |
| Python変数名検証 | ✅ | 予約語36個チェック |
| 危険パターン検出 | ✅ | 7種類の危険パターン検出 |
| タイムアウト制御 | ✅ | 既存PYTHON_COMMANDランナー活用 |
| use_static対応 | ✅ | メモリプーリング機能利用可能 |
| エラーハンドリング | ✅ | exit code, stdout, stderr処理 |
| OpenTelemetry統合 | ✅ | trace/span ID伝播 |

#### 4. コード品質

- ✅ コンパイル成功 (0 errors)
- ✅ cargo fmt適用済み
- ✅ cargo clippy警告なし (-D warnings通過)
- ✅ typify生成コードへのPartialEq追加 (6型)

#### 5. コミット情報

- **コミットID**: `cd552c1`
- **メッセージ**: "implement script runner for Python execution"
- **変更**: 4ファイル、1120行追加、201行削除

---

## Serverless Workflow仕様との整合性

### Phase 1実装での準拠状況

| 項目 | Serverless Workflow v1.0.0 | jobworkerp-rs Phase 1 | 状態 |
|------|---------------------------|----------------------|------|
| `language` | 文字列（必須） | ✅ Rust側でpython/javascript検証 | 完了 |
| `code` | 文字列（codeまたはsource必須） | ✅ 準拠 | 完了 |
| `source` | externalResource参照 | ✅ 基本実装 | 完了 |
| `arguments` | オブジェクト | ✅ 各キーがPython変数化 | 完了 |
| `environment` | オブジェクト | ✅ 環境変数として渡す | 完了 |
| Python固有設定 | 仕様外 | ✅ metadata経由 | 完了 |
| タイムアウト | taskBase.timeout | ✅ PYTHON_COMMANDに委譲 | 完了 |
| セキュリティ制御 | 仕様外 | ⚠️ Phase 2実装予定 | 未完 |

### 使用例

```yaml
document:
  dsl: "1.0.0"
  namespace: example
  name: python-script-demo
  version: "0.1.0"

do:
  - transformData:
      metadata:
        python.version: "3.12"
        python.packages: "numpy,pandas"
      run:
        script:
          language: python
          code: |
            import json
            import numpy as np
            # argumentsの各キーが直接変数として使える
            result = np.array(input_values) * multiplier
            print(json.dumps({"result": result.tolist()}))
          arguments:
            input_values: ${.rawData}  # ランタイム式評価
            multiplier: 2
          environment:
            LOG_LEVEL: "info"
      output:
        as: ${.result}
```

---

## Phase 1: 実装済み機能

### アーキテクチャ

```
┌─────────────────────────────────────────────────────────────┐
│ Workflow YAML/JSON Definition                               │
│  - run.script 設定                                           │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Workflow Executor (app-wrapper)                              │
│  - execute/workflow.rs: WorkflowExecutor                     │
│  - execute/task.rs: TaskExecutor                         │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ ScriptTaskExecutor (実装完了)                                │
│  - definition/workflow/supplement.rs: ValidatedLanguage      │
│  - execute/task/run/script.rs: ScriptTaskExecutor           │
│  - 入出力データのマーシャリング                                 │
│  - ランタイム式評価統合                                        │
│  - セキュリティバリデーション                                   │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ PYTHON_COMMAND Runner (既存)                                 │
│  - runner/src/runner/python.rs                               │
│  - uv仮想環境管理                                             │
│  - スクリプト実行                                              │
└─────────────────────────────────────────────────────────────┘
```

### コンポーネント詳細

#### 1. ScriptTaskExecutor

**場所**: `app-wrapper/src/workflow/execute/task/run/script.rs`

**責務**:
- `ScriptConfiguration` (typify生成enum) のvariant分解
- 引数評価（ランタイム式 → 値）
- Python変数名バリデーション
- 危険パターン検出
- PYTHON_COMMANDランナーへの変換

**主要メソッド**:

| メソッド | 行数 | 責務 |
|---------|------|------|
| `new()` | 11行 | コンストラクタ |
| `to_python_command_args()` | 75行 | スクリプト設定→PYTHON_COMMAND引数変換 |
| `execute()` | 230行 | タスク実行エントリーポイント |
| `is_valid_python_identifier()` | 25行 | Python変数名検証 |
| `sanitize_python_variable()` | 30行 | 危険パターン検出 |

**セキュリティ機能** (Phase 1実装済み):

1. **Python識別子検証**
   - 数字開始チェック
   - 英数字+アンダースコアのみ許可
   - Python予約語36個のチェック

2. **危険パターン検出**
   ```rust
   const DANGEROUS_PATTERNS: &[&str] = &[
       "__import__", "eval(", "exec(", "compile(",
       "open(", "input(", "execfile(",
   ];
   ```

3. **Runtime Expression評価**
   - `UseExpressionTransformer::transform_map()`活用
   - 評価後の値をPythonコードに注入
   - SQL/コードインジェクション対策

#### 2. ValidatedLanguage

**場所**: `app-wrapper/src/workflow/definition/workflow/supplement.rs`

**実装**:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidatedLanguage {
    Python,
    Javascript,
}

impl ValidatedLanguage {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "python" => Ok(Self::Python),
            "javascript" | "js" => Ok(Self::Javascript),
            _ => Err(format!("Unsupported script language: {}", s)),
        }
    }
}
```

#### 3. PythonScriptSettings

**場所**: `app-wrapper/src/workflow/definition/workflow/supplement.rs`

**実装**:
```rust
#[derive(Debug, Clone)]
pub struct PythonScriptSettings {
    pub version: String,
    pub packages: Vec<String>,
    pub requirements_url: Option<String>,
}

impl PythonScriptSettings {
    pub fn from_metadata(metadata: &HashMap<String, String>) -> Result<Self, anyhow::Error> {
        let version = metadata
            .get("python.version")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "3.12".to_string());

        let packages: Vec<String> = metadata
            .get("python.packages")
            .map(|s| {
                s.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let requirements_url = metadata
            .get("python.requirements_url")
            .map(|s| s.to_string());

        // Validation: packages and requirements_url are mutually exclusive
        if !packages.is_empty() && requirements_url.is_some() {
            return Err(anyhow::anyhow!(
                "python.packages and python.requirements_url are mutually exclusive"
            ));
        }

        Ok(Self {
            version,
            packages,
            requirements_url,
        })
    }
}
```

### 入出力データフロー

```
1. Workflow入力
   ↓
2. TaskContext作成 (input: Arc<serde_json::Value>)
   ↓
3. Runtime Expression評価
   - UseExpression::expression() → BTreeMap<String, Arc<Value>>
   - transform_map() → 各argumentsを評価
   ↓
4. Python変数注入コード生成
   - ⚠️ Phase 1: JSON.loads('''...''') 形式 (セキュリティ脆弱性あり)
   - ✅ Phase 2: Base64エンコード方式へ移行予定
   ↓
5. PYTHON_COMMAND実行
   - uv仮想環境作成 (初回のみ、use_static=falseの場合)
   - スクリプト実行
   ↓
6. 結果取得
   - stdout → JSON parse
   - stderr → エラー詳細
   - exit_code → 成否判定
   ↓
7. TaskContext.output更新
```

### エラーハンドリング

Phase 1で実装されたエラー種別:

| エラー種別 | 検出タイミング | ErrorFactory メソッド |
|-----------|---------------|---------------------|
| 言語未サポート | 実行前 | `bad_argument()` |
| 無効な変数名 | 引数準備時 | `bad_argument()` |
| 危険パターン検出 | 引数準備時 | `bad_argument()` |
| ランナー未検出 | 実行前 | `service_unavailable()` |
| スクリプト失敗 | 実行後 | `internal_error()` |
| JSON parseエラー | 実行後 | `internal_error()` |

---

## セキュリティレビューと緊急パッチ計画

**レビュー実施日**: 2025-10-13
**重要度**: 🚨 Critical
**対応期限**: Phase 2開始前（Week 5着手前に完了必須）

### 1. 発見されたセキュリティ脆弱性

#### 1.1 Triple-quoted文字列エスケープの不完全性 (CVE候補)

**現在の実装** (`script.rs:194-200`):
```rust
let json_str = serde_json::to_string(value)?;
script_code.push_str(&format!(
    "{} = json.loads('''{}''')\n",
    key,
    json_str.replace('\\', "\\\\").replace("'''", "\\'\\'\\'")
));
```

**脆弱性の詳細**:
```python
# 攻撃シナリオ例
# 入力: {"cmd": "''')\nimport os; os.system('rm -rf /')#"}

# 生成されるコード（意図しない実行）
cmd = json.loads('''{"cmd": "''')\nimport os; os.system('rm -rf /')#"}''')
# ↑ '''が途中で閉じられ、任意のPythonコードが実行可能
```

**影響範囲**:
- ✅ Serverless Workflow仕様準拠機能に影響
- ❌ 任意のコード実行が可能（RCE: Remote Code Execution）
- ❌ コンテナ脱出の可能性（権限次第）
- 🔍 CVSS v3.1スコア推定: **9.8 (Critical)**
  - 攻撃元区分: ネットワーク
  - 攻撃条件の複雑さ: 低
  - 必要な特権レベル: なし
  - 利用者の関与: 不要

**再現手順**:
```yaml
# 悪意のあるworkflow定義
do:
  - exploit:
      run:
        script:
          language: python
          code: print(payload)
          arguments:
            payload: "''')\nimport os\nos.system('cat /etc/passwd')\n#"
```

#### 1.2 外部スクリプトダウンロードの検証不足

**現在の実装** (`script.rs:120-136`):
```rust
async fn download_script(uri: &str) -> Result<String> {
    let response = reqwest::get(uri).await?;
    if !response.status().is_success() {
        return Err(anyhow!("HTTP status {}", response.status()));
    }
    response.text().await.context("Failed to read response")
}
```

**不足している検証**:
1. ❌ URLスキーマ検証（`file://`, `ftp://`を許可）
2. ❌ ダウンロードサイズ制限なし（DoS攻撃リスク）
3. ❌ TLS証明書検証の明示的確認なし
4. ❌ Content-Type検証なし
5. ❌ タイムアウト設定なし

#### 1.3 危険パターン検出の不完全性

**現在の実装** (`script.rs:95-103`):
```rust
const DANGEROUS_PATTERNS: &[&str] = &[
    "__import__", "eval(", "exec(", "compile(",
    "open(", "input(", "execfile(",
];
for pattern in DANGEROUS_PATTERNS {
    if s.contains(pattern) {
        return Err(anyhow!("Malicious code detected"));
    }
}
```

**バイパス可能な例**:
```python
# 検出される
eval(malicious)

# 検出されない（バイパス）
eval (malicious)              # スペース挿入
getattr(__builtins__, 'eval')()  # 間接呼び出し
exec\t(malicious)             # タブ文字
globals()['__builtins__']['eval']()  # 辞書アクセス
```

### 2. セキュリティ緊急パッチの実装計画

#### 2.1 Base64エンコード方式への移行 (最優先)

**実装期限**: Phase 2 Week 5 Day 1-2 (2営業日)
**優先度**: 🚨 Critical
**担当**: セキュリティ対応チーム

**新実装**:
```rust
use base64::{Engine as _, engine::general_purpose::STANDARD};

impl ScriptTaskExecutor {
    async fn to_python_command_args(
        &self,
        script_config: &workflow::ScriptConfiguration,
        task_context: &TaskContext,
        expression: &std::collections::BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<PythonCommandArgs> {
        let mut script_code = String::new();

        // ... (既存のフィールド抽出ロジック)

        // Step 1: Evaluate runtime expressions
        let evaluated_args = Self::transform_map(
            task_context.input.clone(),
            arguments.clone(),
            expression
        )?;

        // Step 2: Inject evaluated arguments via Base64 encoding
        if let serde_json::Value::Object(ref args_map) = evaluated_args {
            if !args_map.is_empty() {
                script_code.push_str("# Arguments injected via Base64 encoding (secure)\n");
                script_code.push_str("import json\n");
                script_code.push_str("import base64\n\n");

                for (key, value) in args_map {
                    // Security validation
                    Self::sanitize_python_variable(key, value)?;

                    // Serialize and Base64 encode
                    let json_str = serde_json::to_string(value)
                        .context("Failed to serialize argument value")?;
                    let b64_encoded = STANDARD.encode(json_str.as_bytes());

                    // Inject as Python variable (secure)
                    script_code.push_str(&format!(
                        "{} = json.loads(base64.b64decode('{}').decode('utf-8'))\n",
                        key,
                        b64_encoded
                    ));
                }
                script_code.push('\n');
            }
        }

        // Step 3: Append user's script
        match code_or_source {
            Ok(code) => script_code.push_str(code),
            Err(source) => {
                let uri = Self::extract_uri_from_external_resource(source)?;
                let external_code = Self::download_script_secure(&uri).await?;
                script_code.push_str(&external_code);
            }
        }

        Ok(PythonCommandArgs {
            script: Some(python_command_args::Script::ScriptContent(script_code)),
            input_data: None,
            env_vars: environment.clone(),
            with_stderr: true,
        })
    }
}
```

**Serverless Workflow v1.0.0準拠性**:

| 要件 | Triple-quoted方式 | Base64方式 | 備考 |
|------|------------------|-----------|------|
| 変数名で直接アクセス | ✅ | ✅ | 両方とも`message`形式 |
| Runtime expression評価 | ✅ | ✅ | 評価後の値を注入 |
| 任意のJSON型サポート | ✅ | ✅ | string/number/object/array |
| セキュリティ | ❌ | ✅ | Base64はエスケープ不要 |

**検証方法**:
```yaml
# 公式仕様例（dsl-reference.md）
do:
  - log:
      run:
        script:
          language: javascript
          arguments:
            message: ${ .message }
          code: console.log(message)  # ← 変数名で直接アクセス

# jobworkerp-rs (Base64実装)
do:
  - log:
      run:
        script:
          language: python
          arguments:
            message: ${.message}
          code: print(message)  # ← 変数名で直接アクセス可能
```

**生成されるPythonコード例**:
```python
# Base64方式（セキュア）
import json
import base64

# arguments: {"message": "Hello, world!", "count": 42}
message = json.loads(base64.b64decode('IkhlbGxvLCB3b3JsZCEi').decode('utf-8'))
count = json.loads(base64.b64decode('NDI=').decode('utf-8'))

# ユーザーのスクリプト
print(message)  # ✅ "Hello, world!"
print(count)    # ✅ 42
```

**パフォーマンス影響**:
- Base64エンコード: ~200ns
- Base64デコード（Python側）: ~1μs
- 総オーバーヘッド: ~1.2μs
- スクリプト実行時間（数百ms〜数秒）に対する影響: **0.0001%未満（無視可能）**

**依存関係追加**:
```toml
# app-wrapper/Cargo.toml
[dependencies]
base64 = "0.22"  # 最新安定版
```

#### 2.2 外部スクリプトダウンロードの検証強化

**実装期限**: Phase 2 Week 5 Day 2 (0.5営業日)
**優先度**: 🔴 High

**新実装**:
```rust
impl ScriptTaskExecutor {
    /// Download script with comprehensive security validation
    async fn download_script_secure(uri: &str) -> Result<String> {
        // 1. URL schema validation
        let url = reqwest::Url::parse(uri)
            .context("Invalid URL format")?;

        if url.scheme() != "https" {
            return Err(anyhow!(
                "Only HTTPS URLs are allowed for external scripts (got: {})",
                url.scheme()
            ));
        }

        // 2. Download with size limit and timeout
        const MAX_SCRIPT_SIZE: usize = 1024 * 1024; // 1MB
        const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(false)  // Explicitly enable TLS verification
            .timeout(DOWNLOAD_TIMEOUT)
            .build()?;

        let response = client.get(uri)
            .send()
            .await
            .context(format!("Failed to download script from: {}", uri))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to download script: HTTP {} from {}",
                response.status(),
                uri
            ));
        }

        // 3. Content-Type validation (optional but recommended)
        if let Some(content_type) = response.headers().get("content-type") {
            let ct_str = content_type.to_str()
                .context("Invalid Content-Type header")?;

            if !ct_str.starts_with("text/")
                && !ct_str.contains("python")
                && !ct_str.contains("plain") {
                tracing::warn!(
                    "Unexpected Content-Type for script: {} (expected text/* or application/x-python)",
                    ct_str
                );
            }
        }

        // 4. Stream download with size limit
        let bytes = response.bytes()
            .await
            .context("Failed to read response body")?;

        if bytes.len() > MAX_SCRIPT_SIZE {
            return Err(anyhow!(
                "Script size exceeds limit: {} bytes (max: {} bytes)",
                bytes.len(),
                MAX_SCRIPT_SIZE
            ));
        }

        let content = String::from_utf8(bytes.to_vec())
            .context("Script contains invalid UTF-8")?;

        tracing::info!(
            "Downloaded external script from {} ({} bytes)",
            uri,
            content.len()
        );

        Ok(content)
    }
}
```

#### 2.3 危険パターン検出の正規表現ベース実装

**実装期限**: Phase 2 Week 5 Day 3 (1営業日)
**優先度**: 🟡 Medium

**新実装**:
```rust
use regex::Regex;
use once_cell::sync::Lazy;

// Compile regex patterns once at startup
static DANGEROUS_FUNC_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(eval|exec|compile|__import__|open|input|execfile)\s*\("
    ).expect("Invalid regex pattern")
});

static SHELL_COMMAND_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)(os\.system|subprocess\.|commands\.|popen)"
    ).expect("Invalid regex pattern")
});

impl ScriptTaskExecutor {
    /// Enhanced security validation with regex-based pattern detection
    fn sanitize_python_variable(key: &str, value: &serde_json::Value) -> Result<()> {
        // 1. Validate variable name
        if !Self::is_valid_python_identifier(key) {
            return Err(anyhow!(
                "Invalid Python variable name: '{}'. Must be alphanumeric with underscores only.",
                key
            ));
        }

        // 2. Recursively validate string values
        Self::validate_value_recursive(value, 0)?;

        Ok(())
    }

    /// Recursively validate JSON values for security threats
    fn validate_value_recursive(value: &serde_json::Value, depth: usize) -> Result<()> {
        const MAX_DEPTH: usize = 10;
        if depth > MAX_DEPTH {
            return Err(anyhow!("Maximum nesting depth exceeded"));
        }

        match value {
            serde_json::Value::String(s) => Self::validate_string_content(s),
            serde_json::Value::Array(arr) => {
                for item in arr {
                    Self::validate_value_recursive(item, depth + 1)?;
                }
                Ok(())
            }
            serde_json::Value::Object(obj) => {
                for (k, v) in obj {
                    Self::is_valid_python_identifier(k)?;
                    Self::validate_value_recursive(v, depth + 1)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Validate string content for dangerous patterns
    fn validate_string_content(s: &str) -> Result<()> {
        // 1. Dangerous function calls (eval, exec, etc.)
        if DANGEROUS_FUNC_REGEX.is_match(s) {
            return Err(anyhow!(
                "Dangerous function call detected in argument value"
            ));
        }

        // 2. Dunder attribute access (excluding safe ones)
        if s.contains("__") && !s.starts_with("__") && !s.ends_with("__") {
            // Allow common safe patterns
            let safe_dunders = ["__name__", "__doc__", "__version__"];
            if !safe_dunders.iter().any(|&safe| s.contains(safe)) {
                return Err(anyhow!(
                    "Dunder attribute access not allowed in argument value"
                ));
            }
        }

        // 3. Shell command execution patterns
        if SHELL_COMMAND_REGEX.is_match(s) {
            return Err(anyhow!(
                "Shell command execution pattern detected in argument value"
            ));
        }

        Ok(())
    }
}
```

**依存関係追加**:
```toml
# app-wrapper/Cargo.toml
[dependencies]
regex = "1.10"
once_cell = "1.19"
```

### 3. セキュリティテストスイート（緊急版）

**実装期限**: Phase 2 Week 5 Day 3-4 (1.5営業日)
**優先度**: 🔴 High

**テストファイル**: `app-wrapper/tests/script_security_tests.rs`

```rust
#[cfg(test)]
mod security_tests {
    use super::*;

    #[tokio::test]
    async fn test_code_injection_attacks() {
        let test_cases = vec![
            ("triple-quote escape", r#"{"cmd": "''')\nimport os\nos.system('ls')#"}"#),
            ("eval injection", r#"{"cmd": "eval('malicious')"}"#),
            ("exec injection", r#"{"cmd": "exec('malicious')"}"#),
            ("import bypass", r#"{"cmd": "__import__('os').system('ls')"}"#),
            ("getattr bypass", r#"{"cmd": "getattr(__builtins__, 'eval')()"}"#),
        ];

        for (name, malicious_json) in test_cases {
            let value: serde_json::Value = serde_json::from_str(malicious_json).unwrap();
            let result = ScriptTaskExecutor::sanitize_python_variable("cmd", &value);

            assert!(
                result.is_err(),
                "Attack '{}' should be rejected but was accepted",
                name
            );
        }
    }

    #[tokio::test]
    async fn test_base64_encoding_security() {
        // Verify that Base64 encoding prevents code injection
        let dangerous_input = r#"''')\nimport os\nos.system('rm -rf /')#"#;

        let args = serde_json::json!({
            "payload": dangerous_input
        });

        // Generate Python code with Base64 encoding
        let generated_code = generate_base64_injection_code(&args);

        // Verify the dangerous string is not present in raw form
        assert!(!generated_code.contains("import os"));
        assert!(!generated_code.contains("os.system"));
        assert!(!generated_code.contains("rm -rf"));

        // Verify Base64 encoding is used
        assert!(generated_code.contains("base64.b64decode("));
        assert!(generated_code.contains("json.loads("));
    }

    #[tokio::test]
    async fn test_external_script_url_validation() {
        let test_cases = vec![
            ("file:///etc/passwd", false, "file:// should be rejected"),
            ("ftp://malicious.com/script.py", false, "ftp:// should be rejected"),
            ("http://insecure.com/script.py", false, "http:// should be rejected"),
            ("https://trusted.com/script.py", true, "https:// should be accepted"),
        ];

        for (url, should_succeed, reason) in test_cases {
            let result = ScriptTaskExecutor::download_script_secure(url).await;

            if should_succeed {
                // May fail with network error, but shouldn't fail validation
                if let Err(e) = result {
                    let err_msg = format!("{:?}", e);
                    assert!(
                        !err_msg.contains("Only HTTPS URLs are allowed"),
                        "{}",
                        reason
                    );
                }
            } else {
                assert!(result.is_err(), "{}", reason);
            }
        }
    }
}
```

### 4. セキュリティ緊急パッチのロールアウト計画

| Day | タスク | 成果物 | 担当 |
|-----|--------|--------|------|
| 1 | Base64エンコード実装 | `script.rs`修正完了 | コア開発 |
| 2 | 外部URLダウンロード強化 | `download_script_secure()`実装 | コア開発 |
| 3 | 正規表現パターン検出 | `validate_value_recursive()`実装 | セキュリティ |
| 3-4 | セキュリティテスト作成 | `script_security_tests.rs`完成 | QA |
| 5 | 統合テスト・QA | 全テスト通過確認 | QA+コア |

### 5. セキュリティアドバイザリ発行

**タイトル**: Security Advisory: Code Injection Vulnerability in Script Runner (Python)
**発行日**: セキュリティパッチリリース時
**深刻度**: Critical (CVSS 9.8)

**概要**:
jobworkerp-rs v0.18.1以前のScript Runner (Python)機能において、Triple-quoted文字列エスケープの不完全性により、任意のPythonコードが実行可能な脆弱性が発見されました。

**影響を受けるバージョン**:
- v0.18.1以前（Script Runner機能を含むバージョン）

**推奨される対応**:
1. 即座にv0.18.2以降へアップグレード
2. または、Script Runner機能を一時的に無効化

**修正内容**:
- Base64エンコード方式への移行
- 外部スクリプトダウンロードの検証強化
- 正規表現ベースの危険パターン検出

---

## Phase 2: セキュリティ強化と高度な機能

**期間**: 4週間 (Week 5-8)
**ステータス**: Week 5セキュリティ緊急対応準備完了
**更新**: セキュリティレビュー結果を反映し、Week 5を緊急パッチに再割り当て

### Week 5: セキュリティ緊急パッチ実装 (5営業日) 🚨 最優先

**目的**: セキュリティレビューで発見されたCritical脆弱性の即時修正

| Day | タスク | 優先度 | 成果物 |
|-----|--------|--------|--------|
| 1 | Base64エンコード方式実装 | 🚨 Critical | `script.rs:to_python_command_args()`修正 |
| 2 | 外部URLダウンロード検証強化 | 🔴 High | `download_script_secure()`実装 |
| 3 | 正規表現パターン検出実装 | 🟡 Medium | `validate_value_recursive()`実装 |
| 3-4 | セキュリティテスト作成 | 🔴 High | `script_security_tests.rs`完成 |
| 5 | 統合テスト・QA・リリース準備 | 🚨 Critical | v0.18.2リリース候補 |

**完了基準**:
- ✅ Base64エンコード方式が動作し、仕様準拠を維持
- ✅ セキュリティテスト全通過（10ケース以上）
- ✅ 既存機能への影響なし（リグレッションテスト通過）
- ✅ セキュリティアドバイザリ草案完成

### Week 6: セキュリティポリシー制御とバリデーション強化 (5営業日)

**前提**: Week 5の緊急パッチ完了後に着手

#### 1. 入力検証の完全実装

**実装タスク**:

| タスク | 所要時間 | 優先度 |
|--------|----------|--------|
| Pythonバージョン検証 | 0.5日 | 高 |
| パッケージ名検証（PEP 508準拠） | 1日 | 高 |
| requirements_url検証 | 0.5日 | 高 |
| 再帰的値検証の完全実装 | 1日 | 高 |
| 統合テスト作成 | 2日 | 中 |

**実装例**:
```rust
fn validate_python_version(version: &str) -> Result<()> {
    let version_regex = Regex::new(r"^3\.(8|9|10|11|12|13)$")?;
    if !version_regex.is_match(version) {
        return Err(anyhow!(
            "Unsupported Python version: {}. Supported: 3.8-3.13",
            version
        ));
    }
    Ok(())
}

fn validate_package_name(name: &str) -> Result<()> {
    // PEP 508 compliant validation
    let pkg_regex = Regex::new(r"^[a-zA-Z0-9]([a-zA-Z0-9._-]*[a-zA-Z0-9])?$")?;
    if !pkg_regex.is_match(name) {
        return Err(anyhow!("Invalid package name: {}", name));
    }
    Ok(())
}
```

#### 2. metadata経由セキュリティポリシー制御（簡易版）

**目的**: 実行環境のセキュリティ制御を細粒度化（Phase 2範囲を縮小）

**実装内容**:

```yaml
metadata:
  # ファイルシステムアクセス制御（簡易版）
  python.security.filesystem_access: readonly  # none | readonly | full

  # 環境変数制限（簡易版）
  python.security.env_vars_allowlist: "PATH,PYTHONPATH,HOME"
```

**実装タスク**:

| タスク | 所要時間 | 優先度 |
|--------|----------|--------|
| セキュリティポリシーパーサー実装 | 1日 | 中 |
| 環境変数フィルタリング | 1日 | 中 |
| テスト作成 | 1日 | 中 |

**注**: ネットワークアクセス制限とファイルシステムアクセス制限の完全実装はPhase 4に延期

**ファイルシステムアクセス制限の実装例**:

```python
# Auto-generated wrapper script
import sys
import os

# Restrict filesystem access
original_open = open
def restricted_open(file, mode='r', *args, **kwargs):
    allowed_paths = ['/tmp', '/data']
    abs_path = os.path.abspath(file)
    if not any(abs_path.startswith(p) for p in allowed_paths):
        raise PermissionError(f"Access denied: {abs_path}")
    return original_open(file, mode, *args, **kwargs)

__builtins__['open'] = restricted_open

# User's script follows
# ... (injected code)
```

#### 3. リソース制限実装（Phase 4に延期）

**Phase 2範囲縮小**: リソース制限機能はPhase 4の高度な機能に移動

**理由**:
- Week 5の緊急パッチ対応により、Phase 2のスコープを調整
- Unix setrlimit統合はコンテナ環境での動作検証が必要
- セキュリティ緊急対応を最優先するため、優先度を下げる

**Unix setrlimitによる実装例**:

```rust
#[cfg(unix)]
fn apply_resource_limits(settings: &PythonScriptSettings) -> Result<()> {
    use nix::sys::resource::{setrlimit, Resource};

    // CPU time limit (seconds)
    if let Some(cpu_limit) = settings.cpu_limit_seconds {
        setrlimit(Resource::RLIMIT_CPU, cpu_limit, cpu_limit)?;
    }

    // Memory limit (bytes)
    if let Some(mem_limit) = settings.memory_limit_bytes {
        setrlimit(Resource::RLIMIT_AS, mem_limit, mem_limit)?;
    }

    Ok(())
}
```

### Week 7: エラーハンドリング強化とuse_static検証 (5営業日)

**前提**: Week 5-6の成果物完成

#### 実装タスク

| タスク | 所要時間 | 優先度 |
|--------|----------|--------|
| エラー処理マクロ実装（DRY原則） | 1日 | 中 |
| タイムアウトとキャンセルの統合 | 1.5日 | 高 |
| 詳細なエラーメッセージ実装 | 1日 | 中 |
| use_static動作検証 | 1日 | 高 |
| パフォーマンステスト作成 | 0.5日 | 中 |

**エラー処理マクロ例**:
```rust
macro_rules! bail_with_position {
    ($ctx:expr, $factory_method:ident, $msg:expr, $detail:expr) => {{
        let pos = $ctx.position.read().await;
        return Err(workflow::errors::ErrorFactory::new()
            .$factory_method($msg, Some(pos.as_error_instance()), $detail));
    }};
}
```

**use_static動作検証の内容**:
- メモリプーリング機能の動作確認
- 初回実行 vs 2回目以降の起動時間測定（目標: 50%削減）
- メモリ消費量の測定
- プールサイズ設定の最適化
- Base64エンコードオーバーヘッド測定

### Week 8: ドキュメント整備と最終QA (5営業日)

#### ドキュメントタスク

| ドキュメント | 所要時間 | 優先度 |
|-------------|----------|--------|
| セキュリティガイドライン | 2日 | 🚨 Critical |
| ユーザーガイド更新 | 1日 | 高 |
| サンプルワークフロー追加 | 1日 | 高 |
| Phase 2完了レポート作成 | 1日 | 高 |

**成果物**:
- `docs/workflow/script-process-security.md` （最優先）
  - セキュリティベストプラクティス
  - Base64エンコード方式の説明
  - 既知の制限事項
  - セキュリティアドバイザリ詳細
- `docs/workflow/script-process-guide.md` （Phase 1から更新）
- `workflows/examples/script-python-*.yml` （5種類以上）
- Phase 2完了レポート
  - セキュリティ改善の詳細
  - パフォーマンス測定結果
  - Phase 3への移行準備状況

---

## Phase 3: JavaScript サポート

**期間**: 4週間
**開始予定**: Phase 2完了後3-4ヶ月

### 概要

Node.js/Denoベースのスクリプト実行をサポート

### 実装方針

```yaml
run:
  script:
    language: javascript
    code: |
      // argumentsの各キーが直接変数として使える
      const result = input_data.map(x => x * multiplier);
      console.log(JSON.stringify(result));
    arguments:
      input_data: ${.rawData}
      multiplier: 2
    environment:
      NODE_ENV: "production"
```

### 新規コンポーネント

1. **JAVASCRIPT_COMMAND runner** (新規実装)
   - Node.js 環境管理 (nvmまたはfnm)
   - npm / pnpm パッケージ管理
   - スクリプト実行

2. **JavascriptTaskExecutor** (新規実装)
   - ScriptTaskExecutorのJavaScript版
   - Node.js固有の設定処理

3. **JavascriptScriptSettings** (新規実装)
   ```rust
   pub struct JavascriptScriptSettings {
       pub runtime: Runtime,  // Node | Deno
       pub node_version: String,
       pub packages: Vec<String>,
   }
   ```

### 実装タスク分解

| タスク | 所要時間 | 優先度 |
|--------|----------|--------|
| JAVASCRIPT_COMMAND runner実装 | 10日 | 高 |
| JavascriptTaskExecutor実装 | 5日 | 高 |
| Node.js環境管理統合 | 5日 | 高 |
| JavaScript用テストスイート | 5日 | 中 |
| ドキュメント更新 | 3日 | 中 |

---

## Phase 4: 高度な機能

**期間**: 継続的実装
**開始予定**: Phase 3完了後

### サンドボックス実行

**目的**: コンテナレベルの隔離

**実装候補**:
- Firejail統合
- gVisor統合
- Docker-in-Docker実行

### ホットリロード

**目的**: 起動時間の大幅短縮

**実装方針**:
- 仮想環境のプール管理
- 事前ウォームアップ機構
- 動的プールサイズ調整

### 分散実行

**目的**: スケールアウト対応

**実装方針**:
- スクリプト実行専用ワーカープール
- Kubernetes Job統合
- 動的スケーリング

---

## 使用方法

### 基本的な使い方

#### 1. インラインPythonスクリプト

```yaml
document:
  dsl: "1.0.0"
  namespace: example
  name: inline-python
  version: "0.1.0"

do:
  - calculate:
      run:
        script:
          language: python
          code: |
            import json
            # argumentsの各キーが直接変数として使える
            result = x + y
            print(json.dumps({"sum": result}))
          arguments:
            x: 10
            y: 20
```

#### 2. NumPyを使った数値計算

```yaml
do:
  - analyze:
      metadata:
        python.version: "3.12"
        python.packages: "numpy,pandas"
      run:
        script:
          language: python
          code: |
            import json
            import numpy as np
            import pandas as pd

            # argumentsの変数を使用
            df = pd.DataFrame(data)
            result = {
                "mean": float(df["value"].mean()),
                "std": float(df["value"].std())
            }
            print(json.dumps(result))
          arguments:
            data: ${.inputData}
```

#### 3. ランタイム式評価

```yaml
input:
  schema:
    type: object
    properties:
      rawData:
        type: array

do:
  - transform:
      run:
        script:
          language: python
          code: |
            import json
            # argumentsのvaluesは既にランタイム式評価済み
            result = [x * 2 for x in values]
            print(json.dumps(result))
          arguments:
            values: ${.rawData}  # workflow入力のrawDataを評価
```

#### 4. use_static によるパフォーマンス最適化

```yaml
do:
  - highFrequency:
      metadata:
        script.use_static: true  # メモリプーリング有効化
        python.version: "3.12"
        python.packages: "numpy"
      run:
        script:
          language: python
          code: |
            import json
            import numpy as np
            # 2回目以降はuv環境再利用で高速起動
            result = np.array(input).tolist()
            print(json.dumps(result))
          arguments:
            input: ${.data}
```

### トラブルシューティング

#### Q1: スクリプトが実行されない

**確認事項**:
1. `uv`コマンドがインストールされているか
   ```bash
   uv --version
   ```

2. Python指定バージョンが利用可能か
   ```bash
   uv python list
   ```

3. ログでエラーメッセージを確認
   ```bash
   RUST_LOG=debug cargo run
   ```

#### Q2: パッケージインストールに失敗する

**対策**:
1. `python.packages`の記法を確認
   ```yaml
   metadata:
     python.packages: "numpy,pandas"  # カンマ区切り
   ```

2. requirements_url を使用する場合
   ```yaml
   metadata:
     python.requirements_url: "https://example.com/requirements.txt"
   ```

3. パッケージ名が正しいか確認
   ```bash
   uv pip search <package-name>
   ```

#### Q3: タイムアウトエラーが発生する

**対策**:
1. タイムアウト値を増やす
   ```yaml
   do:
     - longTask:
         timeout:
           after:
             minutes: 10
         run:
           script:
             # ...
   ```

2. スクリプトの処理時間を短縮する
   - 不要な処理を削減
   - データ量を制限

#### Q4: use_staticが効かない

**確認事項**:
1. metadata の記法が正しいか
   ```yaml
   metadata:
     script.use_static: true  # 文字列ではなくboolean
   ```

2. Worker設定が同一か
   - 異なるパッケージ構成は別プールエントリ
   - runner_settingsが同一である必要

---

## 変更履歴

| バージョン | 日付 | 変更内容 | 担当者 |
|-----------|------|----------|--------|
| 1.0.0 | 2025-10-13 | 初版作成 | Claude Code |
| 1.1.0 | 2025-10-13 | 公式仕様v1.0.0準拠に修正 | Claude Code |
| 1.2.0 | 2025-10-13 | arguments変数の参照方法を修正 | Claude Code |
| 1.3.0 | 2025-10-13 | use_static設定追加 | Claude Code |
| 1.4.0 | 2025-10-13 | レビュー結果反映版 | Claude Code |
| 1.5.0 | 2025-10-13 | typify運用方式の明確化 | Claude Code |
| 2.0.0 | 2025-10-13 | **Phase 1完了版**: (1) 実装済みコード記述削除 (2) Phase 1完了サマリー追加 (3) Phase 2以降の計画に焦点 (4) 使用方法セクション追加 (5) トラブルシューティング追加 | Claude Code |
| **2.1.0** | **2025-10-13** | **セキュリティレビュー反映版**: (1) セキュリティ脆弱性の詳細分析追加 (2) Base64エンコード方式への移行計画追加 (3) Serverless Workflow v1.0.0準拠性検証 (4) Phase 2スケジュール全面見直し (5) セキュリティ緊急パッチ計画（Week 5）追加 (6) リソース制限機能をPhase 4に延期 (7) セキュリティアドバイザリ草案追加 | **Claude Code** |

---

**以上**
