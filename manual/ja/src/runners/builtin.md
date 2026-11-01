# 組み込みRunner機能

worker_runnerに組み込み定義されている機能を以下に記載します。
各機能のworker.runner_settings、job.argsにはprotobufでそれぞれの機能に必要な値を設定します。[protobuf定義](https://github.com/jobworkerp-rs/jobworkerp-rs/tree/main/runner/protobuf/jobworkerp/runner)は[RunnerService](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/proto/protobuf/jobworkerp/service/runner.proto)によりrunner.data.runner_settings_protoとして取得可能です。メソッドごとの引数・結果スキーマはrunner.data.method_proto_mapから取得できます。

| Runner ID | 名称 | 説明 | 設定項目 |
|----------|------|------|---------|
| COMMAND | コマンド実行 | Worker ホスト上で実行ファイルを直接起動（シェル構文は自動解釈しない） | worker.runner_settings: なし, job.args: コマンド名・引数配列 |
| PYTHON_COMMAND | pythonコマンド実行 | uvを利用したpython script実行 | worker.runner_settings: uv環境設定, job.args: python script、入力など |
| HTTP_REQUEST | HTTPリクエスト | reqwestによるHTTP通信 | worker.runner_settings: base URL, job.args: headers, method, body, path など |
| GRPC | gRPC通信（マルチメソッド） | gRPC unaryおよびサーバストリーミングリクエスト | worker.runner_settings: host, port, TLS設定, デフォルトmethod/metadata/timeout/as_json/proto（すべて任意）等, job.args: method, request, metadata, timeout, as_json, proto（settings側で設定時はすべて省略可）。proto指定時はリフレクション無しでJSON/protobuf変換可能。worker.using: "unary" or "streaming" |
| DOCKER | Dockerコンテナ実行 | docker run相当 | worker.runner_settings: FromImage/Tag, job.args: Image/Cmd など |
| SANDBOX | microVM コマンド実行 | microsandbox による隔離 VM 内での一回実行・対話実行 | 詳細は [SANDBOX Runner](sandbox.md) を参照 |
| LLM | LLM実行（マルチメソッド） | 各種LLMを外部サーバ経由で利用 | 詳細は[LLM](../llm.md)を参照 |
| WORKFLOW | ワークフロー実行（マルチメソッド） | 複数のジョブを定義された順序で実行 | 詳細は[ワークフロー](../workflow.md)を参照 |
| MCP_SERVER | MCPサーバーツール実行 | MCPサーバーのツールを実行 | 詳細は[MCPプロキシ](mcp-proxy.md)を参照 |
| FUNCTION_SET_SELECTOR | FunctionSet一覧 | LLMのツール選択支援のために利用可能なFunctionSetをツール概要付きで一覧表示 | - |

## ストリーミング実行時の失敗

ストリーミング実行の失敗は End の `Trailer.error` で返します。判定規則と code の一覧は[ストリーミング — ストリームエラー契約](../streaming.md#ストリームエラー契約)を参照してください。主な Runner の扱いは次のとおりです。

| Runner | End に `error` を設定する失敗 | 結果データとして返す（`error` なし） |
|---|---|---|
| COMMAND | 起動失敗、stdout/stderr を取得できない、stdin 書き込み失敗（`EXECUTION_FAILED`）。`treat_nonzero_as_error=true` での非 0 終了（`EXECUTION_FAILED`）。キャンセル（`CANCELLED`） | `treat_nonzero_as_error=false` での非 0 終了 |
| HTTP_REQUEST | 接続失敗や本文受信の途中失敗（`TRANSPORT_FAILED`）。不正な method / URL（`INVALID_ARGUMENT`）。キャンセル（`CANCELLED`） | HTTP の 4xx/5xx 応答 |
| GRPC | キャンセル（`CANCELLED`。End の `grpc-status` metadata も維持） | 下流サービスの非 OK status（`grpc-status` / `grpc-message` metadata） |
| LLM | プロバイダのストリームの中断、終端の結果が届く前の終了（`PROVIDER_STREAM_FAILED`）。キャンセル（`CANCELLED`） | — |
| WORKFLOW | workflow の実行エラー（`EXECUTION_FAILED`。Faulted の `WorkflowResult` Data の後に送る） | Faulted / Cancelled の結果 |
| MCP_SERVER | ツール呼び出しの失敗（`EXECUTION_FAILED`）。キャンセル（`CANCELLED`） | ツールの `is_error` |
| Plugin | plugin の `run_stream` のエラー（`EXECUTION_FAILED`）。キャンセル（`CANCELLED`） | — |

Worker の `JobData.timeout` による stream の idle timeout は、どの Runner でも `TIMEOUT` になります。

## GRPC runner の proto ソースについて

GRPC runnerは、呼び出し対象のサービス・メソッドのスキーマを次の2つの方法で解決します。

- **サーバリフレクション**（既定）: 接続先サーバが gRPC reflection を有効にしている場合、proto を指定せずにスキーマを取得できます。
- **外部 proto ソース**: `worker.runner_settings.proto`（接続単位）または `job.args.proto`（リクエスト単位）に proto を指定すると、リフレクション無しでスキーマを解決します。`source` には inline 文字列・`file://`・`http(s)://` を指定できます。

### proto ソースに必要な情報

proto ソースは「gRPC サービスの完全な実装情報」までは必要ありませんが、以下を満たす必要があります。

- **`protoc` でコンパイル可能な、構文的に完全な proto であること。** 内部的に単一ファイルとしてコンパイルするため、**`import` で他ファイルを参照する proto は解決できません**。proto は自己完結（self-contained）である必要があります。
- **呼び出すメソッドの `service { rpc ... }` 宣言が含まれていること。** メソッドはサービス定義から解決するため、メッセージ型だけを定義してサービス／RPC 宣言を省いた proto では解決に失敗します。
- **そのメソッドの入力・出力メッセージ型（および依存するメッセージ型）の定義が含まれていること。** これらは JSON ⇔ protobuf 変換に使用されます。他のサービスや未使用メソッドの定義は不要です。

### proto ソースが不要なケース

リクエストとレスポンスを protobuf の生バイトでやり取りする場合、proto ソースもリフレクションも不要です。

- リクエスト: `job.args.request` を `json_body`（JSON 文字列）ではなく `body`（事前シリアライズ済み protobuf バイト）で渡す。
- レスポンス: `as_json` を指定しない（生バイトのまま受け取る）。

`json_body` の送信や `as_json` による応答の JSON 化を行う場合のみ、スキーマ解決（リフレクションまたは proto ソース）が必要です。

### proto ソースのセキュリティ制約

- `http://`（平文）は既定で拒否され、`allow_insecure_http` を明示的に有効化した場合のみ許可されます。
- `file://` は環境変数 `GRPC_PROTO_ALLOWED_DIR` で許可ディレクトリを設定した場合のみ利用でき、許可ディレクトリ外のパスは拒否されます。
- 取得サイズには上限があります。
