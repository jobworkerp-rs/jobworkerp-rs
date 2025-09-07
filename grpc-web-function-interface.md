# gRPC-Web Function Interface 利用ガイド

## 概要

jobworkerp-rsでは、ブラウザベースのWebクライアント向けにgRPC-Web対応の`FunctionService`インターフェースを提供しています。このインターフェースにより、ブラウザから直接、JSON形式でrunner設定、引数、結果を扱うことができます。

## アーキテクチャと概念

### Function Service の位置づけ

`FunctionService`は、jobworkerp-rsの中核となる`JobService`の高レベルなラッパーとして機能し、ブラウザからのアクセスを容易にするために設計されています。

```mermaid
graph TB
    Browser[ブラウザクライアント<br/>gRPC-Web] --> Function[FunctionService<br/>JSON インターフェース]
    Function --> Job[JobService<br/>protobuf インターフェース]
    Function --> Worker[WorkerService]
    Function --> Runner[RunnerService]
    Job --> Queue[(Job Queue<br/>Redis/Memory)]
    Job --> Storage[(Storage<br/>MySQL/SQLite<br/>&lt;optional&gt;)]
    Queue --> WorkerProcess[Worker Process]
    WorkerProcess --> RunnerImpl[Runner実装<br/>COMMAND/HTTP/Python/etc.]
```

### Functionの概念とJobとの関係

#### 1. Functionとは
- **Function**: Runner（実行エンジン）またはWorker（事前設定済みジョブ）をJSON形式で実行可能にする統一インターフェース
- **Job**: 実際の処理単位。Functionから内部的に生成され、Worker Processで実行される

#### 2. 実行フロー

**Runner指定の場合（`runner_name`）:**
1. `FunctionService.Call()` → 一時的なWorkerを内部作成
2. Workerを使用してJobをエンキュー（`JobService.Enqueue`）
3. Worker Processがジョブを取得・実行
4. 結果をストリーミングで返却

**Worker指定の場合（`worker_name`）:**
1. `FunctionService.Call()` → 既存のWorkerを使用
2. JobをエンキューしWorkerがジョブ実行
3. 結果をストリーミングで返却

#### 3. パフォーマンスの考慮事項

**Runner直接実行の場合:**
- **メリット**: 設定の柔軟性、一回限りの実行に最適
- **デメリット**: 毎回Workerを作成するオーバーヘッド

**Worker事前作成の場合:**
- **メリット**: 高スループット、設定済み実行環境の再利用
- **デメリット**: 事前設定が必要

**推奨事項:**
- **高頻度・大量実行**: 事前にWorkerを作成し`worker_name`で実行
- **一回限り・設定変更頻度高**: `runner_name`で直接実行
- **初期化コストが高いRunner**: Workerの`use_static=true`設定を検討

### ストリーミング対応

Functionは非同期ジョブ実行の特性上、必ずストリーミングレスポンスを返します：

1. **実行開始**: ジョブがエンキューされ、`job_id`を含む初期レスポンス
2. **進行状況**: ランナーによってはリアルタイムな出力（例：長時間のコマンド実行）
3. **完了**: 最終結果と実行メタデータ（`FunctionExecutionInfo`）

### 内部実行詳細

`FunctionService.Call()`の内部動作：

```rust
// Runner名での実行の場合
async fn handle_runner_for_front() {
    // 1. Runnerを名前で検索
    let runner = find_runner_by_name(runner_name);
    
    // 2. 一時的なWorkerデータを作成
    let worker_data = create_worker_data(runner, runner_settings, worker_options);
    
    // 3. Jobをエンキュー（JobService.Enqueueと同等）
    let (job_id, job_result, stream) = setup_worker_and_enqueue_with_json_full_output();
    
    // 4. 結果をFunctionResult形式でストリーミング
    return process_job_result_to_stream();
}
```

この設計により、複雑な内部処理を隠蔽し、ブラウザクライアントから簡単にjobworkerp-rsの機能を利用できます。

## Function Serviceの機能概要

`FunctionService`は以下の主要機能を提供します：

- **Function検索・発見**: ランナーやワーカーの検索
- **Function呼び出し**: JSONペイロードによる非同期ジョブ実行
- **ストリーミング対応**: リアルタイムな結果取得

## protobuf定義

### サービス定義

```proto
service FunctionService {
  // 利用可能なFunction一覧の取得（ストリーミング）
  rpc FindList(FindFunctionRequest) 
    returns (stream FunctionSpecs);
    
  // FunctionSet別のFunction一覧の取得
  rpc FindListBySet(FindFunctionSetRequest) 
    returns (stream FunctionSpecs);
    
  // Function実行（ストリーミング結果）
  rpc Call(FunctionCallRequest) 
    returns (stream FunctionResult);
  
  // ID別のFunction検索
  rpc Find(FindFunctionByIdRequest) 
    returns (OptionalFunctionSpecsResponse);
  
  // 名前別のFunction検索
  rpc FindByName(FindFunctionByNameRequest) 
    returns (OptionalFunctionSpecsResponse);
}
```

### 主要メッセージ

#### FunctionCallRequest（重要）

```proto
message FunctionCallRequest {
  // 実行対象の名前
  oneof name {
    string runner_name = 1;    // ランナー名
    string worker_name = 2;    // ワーカー名
  }
  
  // ランナーパラメータ（runner_nameが指定された場合のみ）
  optional RunnerParameters runner_parameters = 3;

  // ジョブ引数（JSON形式）
  string args_json = 4;

  // 重複実行防止キー
  optional string uniq_key = 5;

  // 実行オプション
  optional FunctionCallOptions options = 6;
}
```

#### RunnerParameters

```proto
message RunnerParameters {
  // ランナー設定（JSON形式）
  string settings_json = 1;
  
  // ワーカーオプション
  WorkerOptions worker_options = 2;
}
```

#### FunctionResult

```proto
message FunctionResult {
  // 実行結果（JSON形式）
  string output = 1;

  // 実行ステータス
  optional ResultStatus status = 2;

  // エラーメッセージ
  optional string error_message = 3;

  // エラーコード
  optional string error_code = 4;

  // 実行情報（ストリーム最後）
  optional FunctionExecutionInfo last_info = 5;
}
```

## gRPC-Web クライアント実装例

### 1. 基本的な接続設定

```typescript
import { FunctionServiceClient } from './generated/jobworkerp/function/service/function_grpc_web_pb';
import { FunctionCallRequest, FindFunctionRequest } from './generated/jobworkerp/function/service/function_pb';

const client = new FunctionServiceClient('http://localhost:8080', null, null);
```

### 2. Function一覧の取得

```typescript
async function listFunctions() {
  const request = new FindFunctionRequest();
  request.setExcludeRunner(false);
  request.setExcludeWorker(false);

  const stream = client.findList(request);
  
  stream.on('data', (response) => {
    console.log('Function:', response.getName());
    console.log('Description:', response.getDescription());
    console.log('Runner Type:', response.getRunnerType());
  });
  
  stream.on('end', () => {
    console.log('Function list completed');
  });
  
  stream.on('error', (err) => {
    console.error('Error:', err);
  });
}
```

### 3. Function実行（ランナー）

```typescript
async function callRunner() {
  const request = new FunctionCallRequest();
  request.setRunnerName('command');
  
  // ランナーパラメータの設定
  const runnerParams = new RunnerParameters();
  runnerParams.setSettingsJson(JSON.stringify({
    // ランナー固有の設定
  }));
  
  const workerOptions = new WorkerOptions();
  workerOptions.setStoreSuccess(true);
  workerOptions.setStoreFailure(true);
  runnerParams.setWorkerOptions(workerOptions);
  
  request.setRunnerParameters(runnerParams);
  
  // ジョブ引数（JSON形式）
  request.setArgsJson(JSON.stringify({
    command: 'echo "Hello World"'
  }));
  
  request.setUniqKey('unique-job-' + Date.now());
  
  const stream = client.call(request);
  
  stream.on('data', (result) => {
    console.log('Output:', result.getOutput());
    console.log('Status:', result.getStatus());
    
    if (result.getLastInfo()) {
      console.log('Job ID:', result.getLastInfo().getJobId());
      console.log('Execution Time:', result.getLastInfo().getExecutionTimeMs());
    }
  });
  
  stream.on('end', () => {
    console.log('Job execution completed');
  });
  
  stream.on('error', (err) => {
    console.error('Execution error:', err);
  });
}
```

### 4. Function実行（ワーカー）

```typescript
async function callWorker() {
  const request = new FunctionCallRequest();
  request.setWorkerName('my-worker');
  
  // ジョブ引数のみ指定（ワーカーの設定は事前に定義済み）
  request.setArgsJson(JSON.stringify({
    input: 'processing data'
  }));
  
  const stream = client.call(request);
  
  stream.on('data', (result) => {
    try {
      const output = JSON.parse(result.getOutput());
      console.log('Processing result:', output);
    } catch (e) {
      console.log('Raw output:', result.getOutput());
    }
  });
  
  stream.on('end', () => {
    console.log('Worker execution completed');
  });
}
```

## 設定とデプロイメント

### サーバー設定

gRPC-Web対応サーバーを起動するには、以下の環境変数を設定してください：

```env
# gRPC-Web有効化
USE_GRPC_WEB=true

# その他の関連設定
MAX_FRAME_SIZE=16777215  # 16MB - 1
GRPC_ADDR=0.0.0.0:9000
```

### サーバー起動

```bash
# gRPC-Web対応サーバーの起動
# USE_GRPC_WEB=trueの環境変数で起動
USE_GRPC_WEB=true ./target/release/grpc-front
```

## 利用可能なRunner Types

以下のRunner Typeが利用可能です：

### COMMAND - シェルコマンド実行

**Runner Settings (settings_json):**
```json
{}
```

**Job Arguments (args_json):**
```json
{
  "command": "echo",
  "args": ["Hello", "World"],
  "with_memory_monitoring": false
}
```

### HTTP_REQUEST - HTTP リクエスト実行

**Runner Settings (settings_json):**
```json
{
  "base_url": "https://api.example.com"
}
```

**Job Arguments (args_json):**
```json
{
  "method": "GET",
  "path": "/users/123",
  "headers": [
    {"key": "Content-Type", "value": "application/json"},
    {"key": "Authorization", "value": "Bearer token"}
  ],
  "queries": [
    {"key": "page", "value": "1"},
    {"key": "limit", "value": "10"}
  ],
  "body": "{\"name\": \"example\"}"
}
```

### PYTHON_COMMAND - Python スクリプト実行

**Runner Settings (settings_json):**
```json
{
  "python_version": "3.11",
  "uv_path": "/usr/bin/uv",
  "packages": {
    "list": ["requests", "pandas", "numpy"]
  }
}
```

**Job Arguments (args_json):**
```json
{
  "script_content": "import sys\nprint('Hello from Python!')\nprint(f'Arguments: {sys.argv[1:]}')",
  "env_vars": {
    "MY_VAR": "example_value"
  },
  "data_body": "input data for script",
  "with_stderr": true
}
```

### DOCKER - Docker コンテナ実行

**Runner Settings (settings_json):**
```json
{
  "from_image": "python:3.11-slim",
  "tag": "latest",
  "env": ["PYTHONPATH=/app"],
  "working_dir": "/app",
  "volumes": ["/tmp"],
  "entrypoint": ["python"]
}
```

**Job Arguments (args_json):**
```json
{
  "cmd": ["-c", "print('Hello from Docker!')"],
  "env": ["DEBUG=1"],
  "working_dir": "/workspace",
  "user": "1000:1000"
}
```

### GRPC_UNARY - gRPC Unary リクエスト

**Runner Settings (settings_json):**
```json
{
  "host": "localhost",
  "port": 9090,
  "tls": false,
  "timeout_ms": 30000,
  "use_reflection": true
}
```

**Job Arguments (args_json):**
```json
{
  "method": "example.v1.ExampleService/GetUser",
  "request": "{\"user_id\": \"123\"}",
  "metadata": {
    "authorization": "Bearer token"
  },
  "timeout": 10000
}
```

### LLM_COMPLETION - LLM テキスト生成

**Runner Settings (settings_json) - Ollama:**
```json
{
  "ollama": {
    "base_url": "http://localhost:11434",
    "model": "llama3.2",
    "system_prompt": "You are a helpful assistant.",
    "pull_model": true
  }
}
```

**Runner Settings (settings_json) - GenAI:**
```json
{
  "genai": {
    "model": "gpt-4o-mini",
    "system_prompt": "You are a helpful assistant."
  }
}
```

**Job Arguments (args_json):**
```json
{
  "prompt": "Write a short story about a robot.",
  "model": "llama3.2",
  "options": {
    "max_tokens": 1000,
    "temperature": 0.7,
    "top_p": 0.9
  },
  "function_options": {
    "use_function_calling": true,
    "use_runners_as_function": true
  },
  "json_schema": "{\"type\": \"object\", \"properties\": {\"title\": {\"type\": \"string\"}, \"content\": {\"type\": \"string\"}}}"
}
```

### SLACK_POST_MESSAGE - Slack メッセージ投稿

**Runner Settings (settings_json):**
```json
{
  "bot_token": "xoxb-your-bot-token",
  "bot_name": "JobWorker Bot"
}
```

**Job Arguments (args_json):**
```json
{
  "channel": "#general",
  "text": "Hello from JobWorker!",
  "attachments": [{
    "color": "good",
    "title": "Job Completed",
    "text": "The job has been completed successfully.",
    "fields": [{
      "title": "Status",
      "value": "Success",
      "short": true
    }]
  }],
  "username": "Custom Bot Name",
  "icon_emoji": ":robot_face:"
}
```

### MCP_SERVER - MCP サーバーツール実行

**Runner Settings (settings_json):**
MCPサーバーの設定は`mcp-settings.toml`ファイルで定義されます

**Job Arguments (args_json):**
```json
{
  "tool_name": "fetch",
  "arg_json": "{\"url\": \"https://api.example.com/data\", \"method\": \"GET\"}"
}
```

### INLINE_WORKFLOW - インライン ワークフロー実行

**Runner Settings (settings_json):**
```json
{}
```

**Job Arguments (args_json):**
```json
{
  "workflow_data": "{\"document\": {\"dsl\": \"1.0.0-alpha\", \"namespace\": \"examples\", \"name\": \"hello-world\", \"version\": \"1.0.0\"}, \"do\": [{\"call\": \"http\", \"with\": {\"method\": \"get\", \"endpoint\": {\"uri\": \"https://api.example.com/hello\"}}}]}",
  "input": "{\"name\": \"World\"}",
  "workflow_context": "{\"timeout\": 30000}"
}
```

### REUSABLE_WORKFLOW - 再利用可能ワークフロー実行

**Runner Settings (settings_json):**
```json
{}
```

**Job Arguments (args_json):**
```json
{
  "workflow_name": "data-processing-workflow",
  "input": "{\"dataset\": \"user_data.csv\", \"filters\": [\"active\", \"verified\"]}",
  "workflow_context": "{\"environment\": \"production\"}"
}
```

### PLUGIN - カスタムプラグイン

**Runner Settings (settings_json):**
プラグイン固有の設定（プラグインの実装による）

**Job Arguments (args_json):**
プラグイン固有の引数（プラグインの実装による）

## エラーハンドリング

```typescript
stream.on('error', (err) => {
  console.error('gRPC Error:', err.code, err.message);
  
  switch (err.code) {
    case grpc.StatusCode.INVALID_ARGUMENT:
      console.error('Invalid arguments provided');
      break;
    case grpc.StatusCode.NOT_FOUND:
      console.error('Function not found');
      break;
    case grpc.StatusCode.UNAVAILABLE:
      console.error('Service unavailable');
      break;
    default:
      console.error('Unexpected error');
  }
});
```

## ベストプラクティス

### 1. 適切なタイムアウト設定

```typescript
const options = new FunctionCallOptions();
options.setTimeoutMs(30000); // 30秒
request.setOptions(options);
```

### 2. 重複実行の防止

重複実行を防ぐ必要のあるジョブに関して、ユニークなキーを設定します。

```typescript
request.setUniqKey(`job-uniq-task-${userId}`);
```

### 3. リソースの適切な管理

```typescript
// ストリームの適切なクリーンアップ
const stream = client.call(request);
const controller = new AbortController();

// タイムアウトでストリームを終了
setTimeout(() => {
  controller.abort();
  stream.cancel();
}, 60000);
```

### 4. 結果のJSONパース

```typescript
stream.on('data', (result) => {
  try {
    const parsedOutput = JSON.parse(result.getOutput());
    // パースされた結果を処理
    handleStructuredOutput(parsedOutput);
  } catch (e) {
    // 生のテキスト出力として処理
    handleRawOutput(result.getOutput());
  }
});
```

## 参考リンク

- [gRPC-Web 公式ドキュメント](https://github.com/grpc/grpc-web)
- [jobworkerp-rs 設定ドキュメント](./deployment/configuration.md)
- [protobuf スキーマ定義](../proto/protobuf/jobworkerp/function/)
