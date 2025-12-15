# AG-UI クライアント利用ガイド

本ドキュメントは、AG-UI Front HTTP API を利用するクライアント実装者向けのガイドです。

## 目次

1. [概要](#概要)
2. [認証](#認証)
3. [API エンドポイント](#api-エンドポイント)
4. [SSE イベント](#sse-イベント)
5. [Human-in-the-Loop (HITL)](#human-in-the-loop-hitl)
6. [エラーハンドリング](#エラーハンドリング)
7. [実装例](#実装例)

---

## 概要

AG-UI Front は [AG-UI プロトコル](https://docs.ag-ui.com/) に準拠した HTTP API を提供し、jobworkerp-rs ワークフローの実行とリアルタイムイベントストリーミングを行います。

### 基本フロー

```text
クライアント                    AG-UI Server
    |                              |
    |-- POST /ag-ui/run ---------->|  ワークフロー実行開始
    |<-------- SSE stream ---------|  イベントストリーム
    |                              |
    |-- GET /ag-ui/stream/{id} --->|  再接続（Last-Event-ID）
    |<-------- SSE stream ---------|
    |                              |
    |-- DELETE /ag-ui/run/{id} --->|  キャンセル
    |<-------- 200 OK -------------|
```

---

## 認証

### Bearer Token 認証

環境変数 `AG_UI_AUTH_TOKENS` でトークンが設定されている場合、全ての `/ag-ui/*` エンドポイントで認証が必要です。

```http
Authorization: Bearer <your-token>
```

認証が無効または未設定の場合、認証なしでアクセス可能です。

### 認証エラー

```json
{
  "error": {
    "code": "UNAUTHORIZED",
    "message": "Invalid or missing authentication token"
  }
}
```

---

## API エンドポイント

### ヘルスチェック

```http
GET /api/health
```

**レスポンス (200 OK):**
```json
{
  "status": "ok",
  "service": "ag-ui-front"
}
```

---

### ワークフロー実行

```http
POST /ag-ui/run
Content-Type: application/json
Authorization: Bearer <token>
```

**リクエストボディ:**

```json
{
  "threadId": "thread-123",
  "runId": "run-456",
  "messages": [
    {
      "role": "user",
      "content": "Hello, please process this request"
    }
  ],
  "tools": [],
  "context": [
    {
      "type": "workflow_definition",
      "workflowName": "my_workflow"
    }
  ],
  "forwardedProps": {
    "workerId": 12345,
    "priority": 10,
    "timeout": 300
  }
}
```

| フィールド | 型 | 必須 | 説明 |
|-----------|------|------|------|
| `threadId` | string | No | 会話スレッドID（未指定時は自動生成） |
| `runId` | string | No | 実行ID（未指定時は自動生成） |
| `messages` | Message[] | No | 入力メッセージ |
| `tools` | Tool[] | No | クライアント定義ツール（HITL用） |
| `context` | Context[] | Yes | コンテキスト（ワークフロー名を含む） |
| `forwardedProps` | object | No | jobworkerp-rs 固有のプロパティ |

**Context タイプ:**

```json
// ワークフロー定義指定
{
  "type": "workflow_definition",
  "workflowName": "my_workflow"
}

// インラインワークフロー
{
  "type": "workflow_inline",
  "workflow": { /* Serverless Workflow YAML/JSON */ }
}

// チェックポイントからの再開
{
  "type": "checkpoint_resume",
  "executionId": "exec-123",
  "position": "/tasks/task1"
}
```

**レスポンス:**

- Content-Type: `text/event-stream`
- カスタムヘッダー:
  - `x-ag-ui-run-id`: 実行ID
  - `x-ag-ui-session-id`: セッションID

SSE ストリームでイベントが配信されます。

---

### ストリーム再接続

```http
GET /ag-ui/stream/{run_id}
Authorization: Bearer <token>
Last-Event-ID: 42
```

接続断後の再接続用エンドポイント。`Last-Event-ID` ヘッダーで指定したID以降のイベントから再開します。

**レスポンス:** SSE ストリーム

---

### HITL メッセージ送信

```http
POST /ag-ui/message
Content-Type: application/json
Authorization: Bearer <token>
```

**リクエストボディ:**

```json
{
  "runId": "run-456",
  "toolCallResults": [
    {
      "toolCallId": "wait_run-456",
      "result": {
        "userInput": "approved",
        "comment": "Looks good to me"
      }
    }
  ]
}
```

| フィールド | 型 | 必須 | 説明 |
|-----------|------|------|------|
| `runId` | string | Yes | 対象の実行ID |
| `toolCallResults` | ToolCallResult[] | Yes | ツール呼び出し結果（**必ず1件**） |

**レスポンス:** SSE ストリーム（再開後のイベント）

---

### ワークフローキャンセル

```http
DELETE /ag-ui/run/{run_id}
Authorization: Bearer <token>
```

**レスポンス (200 OK):**
```json
{
  "status": "cancelled",
  "runId": "run-456"
}
```

---

### 状態取得

```http
GET /ag-ui/state/{run_id}
Authorization: Bearer <token>
```

**レスポンス (200 OK):**
```json
{
  "status": "running",
  "completedTasks": ["task1", "task2"],
  "currentTask": "task3",
  "contextVariables": {
    "result": "intermediate value"
  }
}
```

---

## SSE イベント

### イベント形式

```text
event: EVENT_TYPE
data: {"field": "value", ...}
id: 1
```

### ライフサイクルイベント

| イベント | 説明 |
|---------|------|
| `RUN_STARTED` | ワークフロー実行開始 |
| `RUN_FINISHED` | ワークフロー正常完了 |
| `RUN_ERROR` | ワークフローエラー終了 |
| `STEP_STARTED` | タスク開始 |
| `STEP_FINISHED` | タスク完了 |

**RUN_STARTED:**
```json
{
  "type": "RUN_STARTED",
  "runId": "run-456",
  "threadId": "thread-123",
  "timestamp": 1702345678000
}
```

**RUN_FINISHED:**
```json
{
  "type": "RUN_FINISHED",
  "runId": "run-456",
  "timestamp": 1702345679000,
  "result": { "output": "completed" }
}
```

**RUN_ERROR:**
```json
{
  "type": "RUN_ERROR",
  "runId": "run-456",
  "message": "Task execution failed",
  "code": "TASK_FAILED",
  "timestamp": 1702345679000
}
```

### メッセージイベント（LLM ストリーミング）

| イベント | 説明 |
|---------|------|
| `TEXT_MESSAGE_START` | メッセージ開始 |
| `TEXT_MESSAGE_CONTENT` | メッセージ内容（デルタ） |
| `TEXT_MESSAGE_END` | メッセージ終了 |

**シーケンス例:**
```text
event: TEXT_MESSAGE_START
data: {"type":"TEXT_MESSAGE_START","messageId":"msg-1","role":"assistant","timestamp":1702345678000}
id: 5

event: TEXT_MESSAGE_CONTENT
data: {"type":"TEXT_MESSAGE_CONTENT","messageId":"msg-1","delta":"Hello, ","timestamp":1702345678001}
id: 6

event: TEXT_MESSAGE_CONTENT
data: {"type":"TEXT_MESSAGE_CONTENT","messageId":"msg-1","delta":"how can I help?","timestamp":1702345678002}
id: 7

event: TEXT_MESSAGE_END
data: {"type":"TEXT_MESSAGE_END","messageId":"msg-1","timestamp":1702345678003}
id: 8
```

### ツール呼び出しイベント

| イベント | 説明 |
|---------|------|
| `TOOL_CALL_START` | ツール呼び出し開始 |
| `TOOL_CALL_ARGS` | ツール引数（デルタ） |
| `TOOL_CALL_END` | ツール呼び出し終了 |
| `TOOL_CALL_RESULT` | ツール実行結果 |

### 状態イベント

| イベント | 説明 |
|---------|------|
| `STATE_SNAPSHOT` | 状態スナップショット（全体） |
| `STATE_DELTA` | 状態差分（RFC 6902 JSON Patch） |

---

## Human-in-the-Loop (HITL)

HITL は、ワークフロー実行中にユーザー入力を待機する機能です。

### HITL フロー

```text
クライアント                    AG-UI Server
    |                              |
    |-- POST /ag-ui/run ---------->|
    |<-- RUN_STARTED --------------|
    |<-- STEP_STARTED -------------|
    |<-- TOOL_CALL_START ----------|  toolCallName: "HUMAN_INPUT"
    |<-- TOOL_CALL_ARGS -----------|  現在の出力データ
    |<-- TOOL_CALL_END ------------|
    |   (ストリーム一時停止)        |
    |                              |
    |   [ユーザーが入力]            |
    |                              |
    |-- POST /ag-ui/message ------>|  toolCallResults
    |<-- TOOL_CALL_RESULT ---------|
    |<-- STEP_FINISHED ------------|
    |<-- ... (続行) ---------------|
    |<-- RUN_FINISHED -------------|
```

### HITL イベントシーケンス

1. **TOOL_CALL_START** - `toolCallName: "HUMAN_INPUT"`
2. **TOOL_CALL_ARGS** - 現在のワークフロー出力（JSON文字列）
3. **TOOL_CALL_END** - ツール呼び出し情報の終了

この時点でストリームは一時停止し、クライアントからの入力を待ちます。

### ユーザー入力の送信

```http
POST /ag-ui/message
Content-Type: application/json

{
  "runId": "run-456",
  "toolCallResults": [
    {
      "toolCallId": "wait_run-456",
      "result": {
        "approved": true,
        "comment": "User approved the action"
      }
    }
  ]
}
```

**重要:**
- `toolCallId` は `TOOL_CALL_START` で受信した値と**完全一致**が必要
- `toolCallResults` は**必ず1件のみ**
- `result` は任意のJSON値（ワークフローで使用）

### HITL 再開後のイベント

```text
event: TOOL_CALL_RESULT
data: {"type":"TOOL_CALL_RESULT","toolCallId":"wait_run-456","result":{"approved":true},"timestamp":...}

event: STEP_FINISHED
data: {"type":"STEP_FINISHED","stepId":"task1","timestamp":...}

event: STEP_STARTED
data: {"type":"STEP_STARTED","stepId":"task2","stepName":"next_task","timestamp":...}
...
```

---

## エラーハンドリング

### HTTP エラーレスポンス

```json
{
  "error": {
    "code": "ERROR_CODE",
    "message": "Human-readable error message"
  }
}
```

### エラーコード一覧

| コード | HTTP Status | 説明 |
|--------|-------------|------|
| `SESSION_NOT_FOUND` | 404 | セッションが見つからない |
| `SESSION_EXPIRED` | 410 | セッションが期限切れ |
| `INVALID_INPUT` | 400 | 入力が不正 |
| `WORKFLOW_NOT_FOUND` | 404 | ワークフローが見つからない |
| `WORKFLOW_INIT_FAILED` | 500 | ワークフロー初期化失敗 |
| `TIMEOUT` | 504 | タイムアウト |
| `CANCELLED` | 200 | キャンセル済み |
| `INVALID_SESSION_STATE` | 409 | セッション状態が不正（HITL） |
| `INVALID_TOOL_CALL_ID` | 400 | tool_call_id が不正（HITL） |
| `CHECKPOINT_NOT_FOUND` | 404 | チェックポイントが見つからない |
| `HITL_INFO_NOT_FOUND` | 404 | HITL待機情報が見つからない |
| `INTERNAL_ERROR` | 500 | 内部エラー |

### SSE ストリーム内のエラー

`RUN_ERROR` イベントとして配信されます：

```text
event: RUN_ERROR
data: {"type":"RUN_ERROR","runId":"run-456","message":"Task failed","code":"TASK_FAILED","timestamp":...}
```

---

## 実装例

### TypeScript (fetch + EventSource)

```typescript
interface RunAgentInput {
  threadId?: string;
  runId?: string;
  messages?: Message[];
  context: Context[];
  forwardedProps?: JobworkerpFwdProps;
}

interface AgUiEvent {
  type: string;
  [key: string]: unknown;
}

class AgUiClient {
  private baseUrl: string;
  private token?: string;

  constructor(baseUrl: string, token?: string) {
    this.baseUrl = baseUrl;
    this.token = token;
  }

  private headers(): HeadersInit {
    const headers: HeadersInit = {
      'Content-Type': 'application/json',
    };
    if (this.token) {
      headers['Authorization'] = `Bearer ${this.token}`;
    }
    return headers;
  }

  // ワークフロー実行
  async runWorkflow(
    input: RunAgentInput,
    onEvent: (event: AgUiEvent) => void,
    onError?: (error: Error) => void
  ): Promise<{ runId: string; sessionId: string }> {
    const response = await fetch(`${this.baseUrl}/ag-ui/run`, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify(input),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error?.message || 'Request failed');
    }

    const runId = response.headers.get('x-ag-ui-run-id') || '';
    const sessionId = response.headers.get('x-ag-ui-session-id') || '';

    // SSE ストリーム処理
    const reader = response.body?.getReader();
    const decoder = new TextDecoder();

    if (reader) {
      this.processStream(reader, decoder, onEvent, onError);
    }

    return { runId, sessionId };
  }

  private async processStream(
    reader: ReadableStreamDefaultReader<Uint8Array>,
    decoder: TextDecoder,
    onEvent: (event: AgUiEvent) => void,
    onError?: (error: Error) => void
  ) {
    let buffer = '';

    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        let eventType = '';
        let eventData = '';

        for (const line of lines) {
          if (line.startsWith('event: ')) {
            eventType = line.slice(7);
          } else if (line.startsWith('data: ')) {
            eventData = line.slice(6);
          } else if (line === '' && eventData) {
            try {
              const event = JSON.parse(eventData) as AgUiEvent;
              onEvent(event);
            } catch (e) {
              console.warn('Failed to parse event:', eventData);
            }
            eventType = '';
            eventData = '';
          }
        }
      }
    } catch (error) {
      onError?.(error as Error);
    }
  }

  // HITL メッセージ送信
  async sendMessage(
    runId: string,
    toolCallId: string,
    result: unknown,
    onEvent: (event: AgUiEvent) => void,
    onError?: (error: Error) => void
  ): Promise<void> {
    const response = await fetch(`${this.baseUrl}/ag-ui/message`, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify({
        runId,
        toolCallResults: [{ toolCallId, result }],
      }),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error?.message || 'Request failed');
    }

    const reader = response.body?.getReader();
    const decoder = new TextDecoder();

    if (reader) {
      this.processStream(reader, decoder, onEvent, onError);
    }
  }

  // キャンセル
  async cancel(runId: string): Promise<void> {
    const response = await fetch(`${this.baseUrl}/ag-ui/run/${runId}`, {
      method: 'DELETE',
      headers: this.headers(),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error?.message || 'Cancel failed');
    }
  }

  // 状態取得
  async getState(runId: string): Promise<unknown> {
    const response = await fetch(`${this.baseUrl}/ag-ui/state/${runId}`, {
      headers: this.headers(),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error?.message || 'Get state failed');
    }

    return response.json();
  }
}
```

### 使用例

```typescript
const client = new AgUiClient('http://localhost:8080', 'your-token');

// イベントハンドラ
let pendingToolCall: { id: string; args: string } | null = null;

function handleEvent(event: AgUiEvent) {
  switch (event.type) {
    case 'RUN_STARTED':
      console.log('Workflow started:', event.runId);
      break;

    case 'TEXT_MESSAGE_CONTENT':
      process.stdout.write(event.delta as string);
      break;

    case 'TOOL_CALL_START':
      if (event.toolCallName === 'HUMAN_INPUT') {
        pendingToolCall = { id: event.toolCallId as string, args: '' };
        console.log('\n[Waiting for user input...]');
      }
      break;

    case 'TOOL_CALL_ARGS':
      if (pendingToolCall) {
        pendingToolCall.args += event.delta as string;
      }
      break;

    case 'TOOL_CALL_END':
      if (pendingToolCall) {
        console.log('Current data:', pendingToolCall.args);
        // ここでユーザー入力を収集して sendMessage を呼び出す
      }
      break;

    case 'RUN_FINISHED':
      console.log('\nWorkflow completed');
      break;

    case 'RUN_ERROR':
      console.error('\nWorkflow error:', event.message);
      break;
  }
}

// 実行
async function main() {
  const { runId } = await client.runWorkflow(
    {
      context: [{ type: 'workflow_definition', workflowName: 'my_workflow' }],
      messages: [{ role: 'user', content: 'Process this request' }],
    },
    handleEvent,
    (error) => console.error('Stream error:', error)
  );

  // HITL 入力が必要な場合
  if (pendingToolCall) {
    const userInput = await promptUser('Enter your response:');
    await client.sendMessage(
      runId,
      pendingToolCall.id,
      { userInput },
      handleEvent
    );
  }
}
```

### Python (requests + sseclient)

```python
import requests
import sseclient
import json
from typing import Callable, Optional, Any

class AgUiClient:
    def __init__(self, base_url: str, token: Optional[str] = None):
        self.base_url = base_url
        self.token = token

    def _headers(self) -> dict:
        headers = {"Content-Type": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        return headers

    def run_workflow(
        self,
        workflow_name: str,
        messages: list = None,
        on_event: Callable[[dict], None] = None,
    ) -> tuple[str, str]:
        """ワークフローを実行し、SSEストリームを処理"""
        payload = {
            "context": [{"type": "workflow_definition", "workflowName": workflow_name}],
            "messages": messages or [],
        }

        response = requests.post(
            f"{self.base_url}/ag-ui/run",
            headers=self._headers(),
            json=payload,
            stream=True,
        )
        response.raise_for_status()

        run_id = response.headers.get("x-ag-ui-run-id", "")
        session_id = response.headers.get("x-ag-ui-session-id", "")

        if on_event:
            client = sseclient.SSEClient(response)
            for event in client.events():
                if event.data:
                    data = json.loads(event.data)
                    on_event(data)

        return run_id, session_id

    def send_message(
        self,
        run_id: str,
        tool_call_id: str,
        result: Any,
        on_event: Callable[[dict], None] = None,
    ):
        """HITL メッセージを送信"""
        payload = {
            "runId": run_id,
            "toolCallResults": [{"toolCallId": tool_call_id, "result": result}],
        }

        response = requests.post(
            f"{self.base_url}/ag-ui/message",
            headers=self._headers(),
            json=payload,
            stream=True,
        )
        response.raise_for_status()

        if on_event:
            client = sseclient.SSEClient(response)
            for event in client.events():
                if event.data:
                    data = json.loads(event.data)
                    on_event(data)

    def cancel(self, run_id: str):
        """ワークフローをキャンセル"""
        response = requests.delete(
            f"{self.base_url}/ag-ui/run/{run_id}",
            headers=self._headers(),
        )
        response.raise_for_status()
        return response.json()

    def get_state(self, run_id: str) -> dict:
        """ワークフロー状態を取得"""
        response = requests.get(
            f"{self.base_url}/ag-ui/state/{run_id}",
            headers=self._headers(),
        )
        response.raise_for_status()
        return response.json()


# 使用例
if __name__ == "__main__":
    client = AgUiClient("http://localhost:8080", "your-token")
    pending_tool_call = None

    def handle_event(event: dict):
        global pending_tool_call
        event_type = event.get("type")

        if event_type == "RUN_STARTED":
            print(f"Workflow started: {event['runId']}")

        elif event_type == "TEXT_MESSAGE_CONTENT":
            print(event["delta"], end="", flush=True)

        elif event_type == "TOOL_CALL_START":
            if event["toolCallName"] == "HUMAN_INPUT":
                pending_tool_call = {"id": event["toolCallId"], "args": ""}
                print("\n[Waiting for user input...]")

        elif event_type == "TOOL_CALL_ARGS":
            if pending_tool_call:
                pending_tool_call["args"] += event["delta"]

        elif event_type == "TOOL_CALL_END":
            if pending_tool_call:
                print(f"Current data: {pending_tool_call['args']}")

        elif event_type == "RUN_FINISHED":
            print("\nWorkflow completed")

        elif event_type == "RUN_ERROR":
            print(f"\nWorkflow error: {event['message']}")

    run_id, _ = client.run_workflow("my_workflow", on_event=handle_event)

    if pending_tool_call:
        user_input = input("Enter your response: ")
        client.send_message(
            run_id,
            pending_tool_call["id"],
            {"userInput": user_input},
            on_event=handle_event,
        )
```

---

## 再接続とリカバリ

### Last-Event-ID による再接続

SSE の `id` フィールドを記録し、接続断時に `Last-Event-ID` ヘッダーで再接続することで、イベントの欠落を防げます。

```typescript
let lastEventId = 0;

function handleEvent(event: AgUiEvent, eventId: number) {
  lastEventId = eventId;
  // ... イベント処理
}

async function reconnect(runId: string) {
  const response = await fetch(`${baseUrl}/ag-ui/stream/${runId}`, {
    headers: {
      ...headers,
      'Last-Event-ID': lastEventId.toString(),
    },
  });
  // ストリーム処理を再開
}
```

### セッション状態の確認

再接続前に `/ag-ui/state/{run_id}` でワークフローの状態を確認することを推奨します。

---

## 注意事項

1. **SSE ストリームは単一接続**: 同一 run_id に対して複数のストリーム接続は推奨されません
2. **HITL タイムアウト**: セッションには TTL があり、長時間の待機はセッション期限切れになる可能性があります
3. **イベント順序**: イベントは `id` フィールドで順序保証されています
4. **RUN_FINISHED と RUN_ERROR は排他**: 両方が同時に発生することはありません
