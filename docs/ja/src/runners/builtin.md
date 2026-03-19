# 組み込みRunner機能

worker_runnerに組み込み定義されている機能を以下に記載します。
各機能のworker.runner_settings、job.argsにはprotobufでそれぞれの機能に必要な値を設定します。[protobuf定義](https://github.com/jobworkerp-rs/jobworkerp-rs/tree/main/runner/protobuf/jobworkerp/runner)は[RunnerService](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/proto/protobuf/jobworkerp/service/runner.proto)によりrunner.data.runner_settings_protoとして取得可能です。メソッドごとの引数・結果スキーマはrunner.data.method_proto_mapから取得できます。

| Runner ID | 名称 | 説明 | 設定項目 |
|----------|------|------|---------|
| COMMAND | コマンド実行 | シェルコマンドを実行 | worker.runner_settings: なし, job.args: コマンド名・引数配列 |
| SLACK_POST_MESSAGE | Slackメッセージ投稿 | Slackチャンネルへのメッセージ投稿 | worker.runner_settings: Slackトークン, job.args: チャンネル、メッセージテキスト等 |
| PYTHON_COMMAND | pythonコマンド実行 | uvを利用したpython script実行 | worker.runner_settings: uv環境設定, job.args: python script、入力など |
| HTTP_REQUEST | HTTPリクエスト | reqwestによるHTTP通信 | worker.runner_settings: base URL, job.args: headers, method, body, path など |
| GRPC | gRPC通信（マルチメソッド） | gRPC unaryおよびサーバストリーミングリクエスト | worker.runner_settings: host, port, TLS設定等, job.args: メソッド名, リクエスト, using: "unary" or "streaming" |
| DOCKER | Dockerコンテナ実行 | docker run相当 | worker.runner_settings: FromImage/Tag, job.args: Image/Cmd など |
| LLM | LLM実行（マルチメソッド） | 各種LLMを外部サーバ経由で利用 | 詳細は[LLM](../llm.md)を参照 |
| WORKFLOW | ワークフロー実行（マルチメソッド） | 複数のジョブを定義された順序で実行 | 詳細は[ワークフロー](../workflow.md)を参照 |
| MCP_SERVER | MCPサーバーツール実行 | MCPサーバーのツールを実行 | 詳細は[MCPプロキシ](mcp-proxy.md)を参照 |
| FUNCTION_SET_SELECTOR | FunctionSet一覧 | LLMのツール選択支援のために利用可能なFunctionSetをツール概要付きで一覧表示 | - |
