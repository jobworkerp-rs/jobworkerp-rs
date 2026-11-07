# AG-UI クライアント利用ガイド

このガイドは AG-UI 1.0 の標準 RunAgentInput と SSE event を使うクライアント向けです。AG-UI Front はサーバーに登録された endpoint profile を提供します。接続先 URL が実行方式を選び、request body から worker、workflow、guardrail 設定は変更できません。

## 接続先の選択

サーバー管理者は `AG_UI_PROFILES` に endpoint path、登録済み worker 名、実行方式を登録します。`streaming` は登録済み LLM/chat worker をリアルタイム実行し、`workflow` は guardrail workflow の公開 output を完了後に返します。実際の設定例は README の endpoint/profile 表を参照してください。

```http
POST <登録済みprofile path>
Content-Type: application/json
Authorization: Bearer <token>
```

## RunAgentInput

送信 body は AG-UI 1.0 schema に従います。`threadId`、`runId`、`messages` は必須です。optional fields は標準定義に従い、Context は `{ "description": "...", "value": "..." }` です。`tools` は標準 Tool definitions、`resume` は標準 ResumeEntry の配列です。`workflow_inline`、worker 指定、設定上書きなど独自形式は使えません。

```json
{
  "threadId": "thread-123",
  "runId": "run-456",
  "messages": [{ "id": "msg-1", "role": "user", "content": "Hello" }],
  "tools": []
}
```

`state` と `forwardedProps` は client data です。サーバー設定や checkpoint を変更する権限はありません。workflow profile は workflow が明示的に mapping した共有 state だけを返します。

workflow worker は完了時に公開契約に従う output を返します。`messages` は追加 assistant message 群として必須、`state` と `result` は任意です。workflow の中間 output や internal context をこの object に無条件 merge しないでください。

```json
{
  "messages": [
    { "id": "assistant-1", "role": "assistant", "content": "Guardrail-checked answer" }
  ],
  "state": { "lastAnswer": "Guardrail-checked answer" },
  "result": { "approved": true }
}
```

## Event stream

POST response は `text/event-stream` です。stream の最初は `RUN_STARTED`、終了は `RUN_FINISHED` または `RUN_ERROR` です。各 event は AG-UI 1.0 schema の field を使います。再接続は `GET /ag-ui/stream/{run_id}` と `Last-Event-ID` header を利用します。`streaming` profile の run を `DELETE /ag-ui/run/{run_id}` でキャンセルすると、POST response と再接続の stream はどちらも `RUN_ERROR` code `CANCELLED` で終了します。読み込みが遅いクライアントは、キャンセル後の event を POST response で受け取れないことがあります。終了 event を受け取らずに stream が閉じた場合は、`Last-Event-ID` を付けて再接続してください。すでに終了した run への `DELETE` は何も変更せず、成功を返します。state 取得 endpoint は workflow 内部変数を返さず、公開対象に限定されます。

### Tool ownership

`RunAgentInput.tools` で渡した client tools はクライアントが実行します。Front は `TOOL_CALL_START` / `TOOL_CALL_ARGS` / `TOOL_CALL_END` を送り run を終了します。クライアントは結果を `role: "tool"`、同じ `toolCallId` の message として次の run に含めます。`streaming` profile では、ToolMessage の `error` は失敗した tool result として LLM に渡り、content の部分結果も残ります。同 profile の ToolMessage の content はテキストだけを受け付け、画像などのメディア part を含む request は拒否されます。

jobworkerp FunctionSets は server tools です。workflow profile の workflow 内で別 LLM task として構成します。同じ LLM 呼び出しへ client tools と server FunctionSets を同時指定できません。server tool の結果は run 終了前に履歴と `TOOL_CALL_RESULT` に反映されます。

## Resume

HITL/checkpoint を再開するときは、同じ `threadId` と profile URL を使い、新しい `runId` と標準 `resume` entry 配列を送ります。checkpoint 識別子はサーバー内部に保持されます。要求の serialize や Redis connection 取得など、queue write 開始前の失敗と確認できる場合は interrupt が未回答のまま残り、同じ回答を再送できます。Redis が queue write を受け付けた後に応答が失われた場合、enqueue 結果は不明です。job はすでに queue 済み、実行中、または完了済みの可能性があります。response run は `RUN_STARTED` の後に `RUN_ERROR` code `ENQUEUE_OUTCOME_UNKNOWN` で終了します。同じ interrupt を自動再送しないでください。復旧方法を決める前に、運用担当者が該当 run の structured server log から job ID を確認します。client tools の継続には resume ではなく次 run の ToolMessage を使います。
