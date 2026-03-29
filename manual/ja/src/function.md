# Function / FunctionSet

## Functionとは

Functionは、**Runner**と**Worker**を統一的なインターフェースで扱う抽象層です。Runnerの設定、Workerの作成、Jobのエンキューを個別に行う代わりに、関数名とJSON引数を指定するだけでタスクを実行できます。

Functionの参照方法は2つあります：

- **Runner名で指定**: Runner名（とオプションの設定）を指定して直接実行。内部で一時的なWorkerが作成される。
- **Worker名で指定**: 既存のWorker名を指定し、事前設定済みの設定で実行。

MCP/Pluginランナーのように複数ツールを持つ場合は、特定のツールに絞り込むことも可能です。マルチメソッドランナー（例：LLM）の場合は、特定のメソッド（例：`"chat"`）を選択できます。

## FunctionSetとは

FunctionSetは複数のFunctionを名前付きコレクションとしてグループ化します。LLMやMCPの利用において**どのツールを使えるか**を制御する重要な概念です：

- **LLMツール呼び出し**: FunctionSet名を指定してLLMが使えるツールを制限し、コンテキストを効率的に保つ
- **MCPサーバー**: MCPサーバー設定でFunctionSet名を指定し、外部クライアントに公開するツールを制御
- **AutoSelection**: 多数のFunctionSetがある場合、ツール実行前にLLMが最適なセットを自動選択できる（[AutoSelection](#autoselectionfunctionsetの階層的選択)を参照）

FunctionSetは名前、説明、カテゴリ、対象Functionのリストで定義されます。

### FunctionSetService

FunctionSetServiceはFunctionSetを管理するCRUD操作を提供します：

| RPC | 説明 |
|-----|------|
| `Create` / `Update` / `Delete` | FunctionSetの管理 |
| `Find` / `FindByName` | IDまたは名前でFunctionSetを検索 |
| `FindDetail` / `FindDetailByName` | 各ターゲットのメタデータ（FunctionSpecs）を展開して検索 |
| `FindList` | 全FunctionSetを一覧取得 |
| `Count` | FunctionSetの総数を取得 |

## Functionの実行

### FunctionService.Call

Functionを実行する主要な方法は`Call` RPCです。関数名とJSON引数を受け取り、結果のストリームを返します。

指定する名前の種類によって動作が分岐します：

| 名前の種類 | 動作 |
|-----------|------|
| `runner_name` | 指定されたRunnerで一時的なWorkerを作成。オプションで`runner_parameters`（設定JSON、WorkerOptions）を指定して実行を構成可能。 |
| `worker_name` | 既存のWorkerを名前で検索し、事前設定済みの設定でJobをエンキュー。 |

その他のリクエストフィールド：

| フィールド | 説明 |
|-----------|------|
| `args_json` | JSON文字列としてのJob引数 |
| `options` | オプション：timeout_ms、streaming、metadata |
| `uniq_key` | オプション：重複排除キー |

### Functionの検索

| RPC | 説明 |
|-----|------|
| `FindList` | 全Functionを一覧取得（RunnerまたはWorkerの除外オプション付き） |
| `FindListBySet` | 特定のFunctionSetに属するFunctionを一覧取得 |
| `FindByName` | runner_nameまたはworker_nameでFunctionを検索 |
| `Find` | FunctionUsing（FunctionId + optional using）でFunctionを検索 |

### Workerの作成

| RPC | 説明 |
|-----|------|
| `CreateWorker` | Runner名/IDと設定からWorkerを作成 |

> **注意**: ワークフローWorkerの作成には `WORKFLOW.create` メソッドを使用してください（`JobService.Enqueue` に `using="create"` を指定）。詳細は[Workflow Runner](workflow.md)を参照してください。

## LLM Function Calling

LLM Runnerの`chat`メソッドでFunctionSetを指定すると、LLMにツールとして提供できます。各FunctionはLLMが呼び出し可能なツール定義に変換されます。

### 自動呼び出しモード

- **`is_auto_calling: true`**: LLMが返すツール呼び出しをシステムが自動実行し、結果をフィードバック（最大10ラウンド）
- **`is_auto_calling: false`**（デフォルト）: ツール呼び出しをクライアントに返却し、確認後に実行

### ツール名の命名規則

- 単一メソッドRunner: Runner名そのまま（例：`"http_request"`）
- マルチメソッドRunner: `"runner_name___method_name"`（アンダースコア3つ区切り）

LLMの設定やプロバイダーの詳細は[LLM Runner](llm.md)を参照してください。

### AutoSelection（FunctionSetの階層的選択）

多数のツールが登録されている場合、すべてをLLMに提供するとコンテキストを浪費し、性能が低下する可能性があります。AutoSelectionは2フェーズのアプローチでこの問題に対処します：

1. **フェーズ1 - セット選択**: 全FunctionSetが`select_toolset_{セット名}`という疑似ツールとしてLLMに提示される。LLMがタスクに最適なセットを選択。
2. **フェーズ2 - ツール実行**: 選択されたFunctionSetのツールのみがLLMに提供され、実際の実行が行われる。

AutoSelectionを有効にするには、chatリクエストで`auto_select_function_set: true`を設定します。

## MCPサーバーバックエンド

`MCP_ENABLED=true`に設定すると、jobworkerp-rsのWorkerとRunnerをMCP（Model Context Protocol）ツールとして外部に公開し、外部のLLMエージェントやMCPクライアントから利用可能にします。

- FunctionメタデータがMCPツール定義に変換される
- ツール引数は統合スキーマ`{ "settings": {...}, "arguments": {...} }`に従う
- MCPサーバー設定でFunctionSet名を指定し、公開ツールを制限可能

MCPプロキシ設定（外部MCPサーバーをRunnerとして利用）については、[MCPプロキシ](runners/mcp-proxy.md)を参照してください。

## 関連ドキュメント

- [LLM Runner](llm.md) - LLMプロバイダーの設定とchatメソッド
- [gRPC-Web API](grpc-web-api.md) - Function呼び出し用のブラウザアクセス可能なJSON API
- [MCPプロキシ](runners/mcp-proxy.md) - 外部MCPサーバーをRunnerとして利用
- [ストリーミング](streaming.md) - Function呼び出しのストリーミング実行
