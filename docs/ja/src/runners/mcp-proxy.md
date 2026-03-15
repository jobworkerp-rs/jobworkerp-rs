# MCPプロキシ機能

Model Context Protocol (MCP) は、LLMアプリケーションとツール間の標準通信プロトコルです。jobworkerp-rsのMCPプロキシ機能を使用することで、以下のことが可能になります：

- 各種MCPサーバーが提供する機能（LLM、時間情報取得、Webページ取得など）をRunnerとして実行
- 非同期ジョブとしてMCPツールを実行し、結果を取得
- 複数のMCPサーバーを設定し、異なるツールをそれぞれ利用

## MCPサーバー設定

MCPサーバーの設定はTOMLファイルで行います。以下は設定例です：

```toml
[[server]]
name = "time"
description = "timezone"
transport ="stdio"
command = "uvx"
args = ["mcp-server-time", "--local-timezone=Asia/Tokyo"]

[[server]]
name = "fetch"
description = "fetch web page as markdown from web"
transport ="stdio"
command = "uvx"
args = ["mcp-server-fetch"]

# SSEトランスポートの例
#[[server]]
#name = "test-server"
#transport ="sse"
#url = "http://localhost:8080"
```

設定ファイルは環境変数 `MCP_CONFIG` に指定したパスに配置します。環境変数を設定していない場合は、デフォルトで `mcp-settings.toml` が使用されます。(無い場合は無視されます)

> **注意**: 設定ファイルはTOML、JSON、YAMLいずれの形式でも記述可能です。TOML → JSON → YAML の順でパースを試みます。

## MCPプロキシの使用例

1. MCPサーバー設定ファイルを準備する
2. worker作成時にrunner_idに特定のMCPサーバーの数値IDを指定（`jobworkerp-client runner list` コマンドで利用可能なMCPサーバーを確認できます）
3. ジョブ実行時の引数にtool_nameとarg_jsonを指定する

```shell
# まず利用可能なrunner-idを確認する
$ ./target/release/jobworkerp-client runner list
# ここで特定のMCPサーバーのIDを確認（例: 3 が "time" サーバー）

# MCPサーバーを使用するワーカーを作成（例：時間情報取得）
# runner-idには上記で確認したMCPサーバーのID番号を指定
$ ./target/release/jobworkerp-client worker create --name "TimeInfo" --description "" --runner-id <runner id> --response-type DIRECT --settings '' --use-static

# ジョブを実行して現在の時間情報を取得
$ ./target/release/jobworkerp-client job enqueue --worker "TimeInfo" --args '{"tool_name":"get_current_time","arg_json":"{\"timezone\":\"Asia/Tokyo\"}"}'

# Webページ取得の例
$ ./target/release/jobworkerp-client worker create --name "WebFetch" --description "" --response-type DIRECT --settings '' --runner-id <runner id>
$ ./target/release/jobworkerp-client job enqueue --worker "WebFetch" --args '{"tool_name":"fetch","arg_json":"{\"url\":\"https://example.com\"}"}'
```

MCPサーバーからの応答はジョブ結果として取得でき、response_typeの設定に従って直接または非同期に処理できます。

> **注意**: PYTHON_COMMANDやMCPサーバプロキシによるMCPサーバツールの実行(stdio)など初期化に時間がかかるRunnerは、worker作成時に`use_static`オプションを`true`に設定することで、ツール実行ごとにMCPサーバプロセスを初期化することなく再利用できます。これによりメモリ使用量は増加しますが、実行速度が向上します。

詳細なMCPプロトコル仕様については、[公式ドキュメント](https://modelcontextprotocol.io/)を参照してください。
上記で利用しているMCPサーバサンプルに関しては [公式ドキュメント](https://github.com/modelcontextprotocol/servers)を参照してください。
