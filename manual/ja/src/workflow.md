# Workflow Runner

## 概要

Workflow Runnerは、定義された順序で複数のジョブを実行したり、再利用可能なワークフローを実行したりするための機能です。[Serverless Workflow](https://serverlessworkflow.io/) ([DSL仕様 v1.0.0](https://github.com/serverlessworkflow/specification/blob/v1.0.0/dsl.md))のワークフローロジック構築方法（タスクの順序実行、分岐、ループ、エラーハンドリング等のフロー制御とランタイム式）に準拠しつつ、jobworkerp-rs独自のジョブ実行基盤に統合した拡張DSLです。([jobworkerp-rs拡張スキーマ](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/runner/schema/workflow.yaml))

## WORKFLOW ランナー（統合版）

`WORKFLOW`ランナーはマルチメソッドランナーとして、jobworkerp-rsを通じてワークフロー機能を提供します。`using`パラメータで実行メソッドを指定します。

### メソッド

#### run（デフォルト）

ワークフロー定義を実行します。(`using`が指定されない場合のデフォルトメソッドです)

- ワークフローは`worker.runner_settings`に事前設定するか、ジョブ引数で実行時に指定可能
- 両方指定された場合、ジョブ引数が設定より優先される
- jq構文（`${}`）とLiquidテンプレート構文（`$${}`）の両方を使用した動的変数展開をサポート
- リアルタイム監視のためのストリーミング実行をサポート
- チェックポイントベースの再開をサポート

#### create

ワークフロー定義から新しいWORKFLOWランナーワーカーを作成します。

- 内部的には`WorkerService/Create`をRunner=WORKFLOWで呼び出すヘルパーメソッドです。ワークフロー定義のバリデーションと`WorkerData`の構築を自動化しますが、作成されるWorkerは通常のWorkerと同じストレージに保存されます（特別な保存先はありません）。
- `WorkerService/Create`で`runner_id`にWORKFLOW、`runner_settings`にワークフロー定義を設定して手動でWorkerを作成するのと同等です。
- 作成されたワーカーは`run`メソッドで異なる入力で繰り返し実行可能

### ランナー設定（WorkflowRunnerSettings）

`run`メソッド用に`worker.runner_settings`で設定するフィールド：

| フィールド | 型 | 説明 |
|-----------|------|------|
| `workflow_url` | string (oneof) | ワークフロー定義ファイルへのパスまたはURL（JSON/YAML） |
| `workflow_data` | string (oneof) | インラインのワークフロー定義（JSON/YAML文字列） |
| `workflow_context` | string (省略可) | コンテキスト変数（JSONオブジェクト文字列）。設定した場合、実行時引数の`workflow_context`は無視される |

> `workflow_url`と`workflow_data`は排他的です（`oneof workflow_source`）。ジョブ引数でワークフローソースを指定する場合、設定は省略可能です。

### ジョブ引数

#### `run`メソッド用（WorkflowRunArgs）

| フィールド | 型 | 説明 |
|-----------|------|------|
| `workflow_url` | string (oneof, 省略可) | URL/パスでワークフローソースを上書き |
| `workflow_data` | string (oneof, 省略可) | インライン定義でワークフローソースを上書き |
| `input` | string | ワークフローコンテキストへの入力（JSONまたはプレーン文字列） |
| `workflow_context` | string (省略可) | 追加のコンテキスト情報（JSON）。ランナー設定側に`workflow_context`がある場合は無視される |
| `execution_id` | string (省略可) | ワークフロー実行インスタンスの一意識別子 |
| `from_checkpoint` | Checkpoint (省略可) | 特定のチェックポイントから再開するためのデータ |

#### `create`メソッド用（CreateWorkflowArgs）

| フィールド | 型 | 説明 |
|-----------|------|------|
| `workflow_url` | string (oneof) | ワークフローソースのURL/パス |
| `workflow_data` | string (oneof) | インラインのワークフロー定義 |
| `name` | string | 作成するワーカーの名前 |
| `worker_options` | WorkerOptions (省略可) | ワーカー設定（リトライポリシー、チャネル、レスポンスタイプなど） |

### 結果タイプ

#### WorkflowResult（`run`メソッド用）

| フィールド | 型 | 説明 |
|-----------|------|------|
| `id` | string | ワークフロー実行の一意識別子（UUID v7） |
| `output` | string | ワークフロー実行の結果（JSONまたはプレーン文字列） |
| `position` | string | JSON Pointer形式の実行位置 |
| `status` | WorkflowStatus | 実行ステータス: `Completed`, `Faulted`, `Cancelled`, `Running`, `Waiting`, `Pending`（通常は最終結果に出現しない内部ステータス） |
| `error_message` | string (省略可) | ワークフローが失敗した場合のエラー詳細 |

#### CreateWorkflowResult（`create`メソッド用）

| フィールド | 型 | 説明 |
|-----------|------|------|
| `worker_id` | WorkerId (省略可) | 作成されたワーカーの識別子 |

## ワークフロー例

以下は、ファイルをリストアップし、ディレクトリをさらに処理するワークフローの例です：
(`$${...}`: Liquid テンプレート、`${...}`: jq)

```yaml
document:
  name: ls-test
  namespace: default
  title: Workflow test (ls)
  version: 0.0.1
  dsl: 0.0.1
input:
  schema:
    document:
      type: string
      description: file name
      default: /
do:
  - ListWorker:
      run:
        function:
          runnerName: COMMAND
          arguments:
            command: ls
            args: ["${.}"]
          options:
            channel: workflow
            useStatic: false
            storeSuccess: true
            storeFailure: true
      output:
        as: |-
          $${
          {%- assign files = stdout | newline_to_br | split: '<br />' -%}
          {"files": [
          {%- for file in files -%}
          "{{- file |strip_newlines -}}"{% unless forloop.last %},{% endunless -%}
          {%- endfor -%}
          ] }
          }
  - EachFileIteration:
      for:
        each: file
        in: ${.files}
        at: ind
      do:
        - ListWorkerInner:
            if: |-
              $${{%- assign head_char = file | slice: 0, 1 -%}{%- if head_char == "d" %}true{% else %}false{% endif -%}}
            run:
              function:
                runnerName: COMMAND
                arguments:
                  command: ls
                  args: ["$${/{{file}}}"]
                options:
                  channel: workflow
                  useStatic: false
                  storeSuccess: true
                  storeFailure: true
```

## ワークフローランナーの利用方法

### gRPC APIを使用する場合

WORKFLOWランナーはgRPCの`JobService.Enqueue`（ストリーミングの場合は`EnqueueForStream`）で利用します：

- `worker_name`: WORKFLOWランナーで作成したワーカーの名前
- `args`: シリアライズされた`WorkflowRunArgs`または`CreateWorkflowArgs`（protobuf）
- `using`: `"run"`（デフォルト、省略可）または`"create"`

### jobworkerp-client CLIを使用する場合

上記のworkflow定義をワーカープロセスと同一ディレクトリに`ls.yaml`として保存した場合：

```shell
# ワークフローを事前設定したWORKFLOWワーカーの作成（runメソッド）
$ ./target/release/jobworkerp-client worker create \
    --name "MyWorkflow" \
    --runner-name WORKFLOW \
    --response-type DIRECT \
    --settings '{"workflow_url":"./ls.yaml"}'

# 入力を指定してワークフローを実行
$ ./target/release/jobworkerp-client job enqueue \
    --worker "MyWorkflow" \
    --args '{"input":"/home"}'

# 実行時にワークフローを上書きして実行
$ ./target/release/jobworkerp-client job enqueue \
    --worker "MyWorkflow" \
    --args '{"workflow_url":"./another.yaml", "input":"/tmp"}'

# 再利用可能なワークフローワーカーの作成（createメソッド）
$ ./target/release/jobworkerp-client job enqueue \
    --worker "MyWorkflow" \
    --using create \
    --args '{"workflow_data":"<YAMLまたはJSONワークフロー定義>", "name":"new-workflow-worker"}'

# ワーカーを作成せずに直接ワークフローを実行する方法（ショートカット）
$ ./target/release/jobworkerp-client job enqueue-workflow -i '/path/to/list' -w ./ls.yaml
# このコマンドは内部的に一時的なワーカーを自動的に作成し、ワークフローを実行し、workerを削除します
```

> **注意**: `workflow_url`には`https://` などのURL以外にもローカルファイルシステム上のファイルの絶対/相対パスも指定できます。相対パスの場合はjobworkerp-workerの実行ディレクトリからの相対パスを指定する必要があります。

## 変数展開

ワークフローでは2種類の変数展開をサポートしています：

- **jq構文** (`${...}`): JSONデータ操作のための標準的なjq式
- **Liquidテンプレート構文** (`$${...}`): 文字列操作や制御フローのための[Liquid](https://shopify.github.io/liquid/)テンプレート式

### 環境変数の参照

jq構文では、jaqクレートの組み込み `env` 関数を通じてプロセスの環境変数を参照できます：

```yaml
# jq構文での環境変数参照
botToken: ${env.SLACK_BOT_TOKEN}
apiKey: ${env.API_KEY}
```

> **注意**: 環境変数の参照はjq構文（`${...}`）でのみ利用可能です。Liquidテンプレート構文（`$${...}`）では環境変数を直接参照できません。

### コンテキスト変数

ワークフロー実行時に `input` とは別に、`workflow_context` パラメータでグローバル変数を渡すことができます。渡されたJSONオブジェクトの各キーがワークフロー全体でトップレベル変数として参照可能になります。

#### 入力方法

**ランナー設定（WorkflowRunnerSettings）で指定する場合：**

ワーカー作成時に `runner_settings` の `workflow_context` にJSON文字列を設定します。設定側で指定した場合、実行時引数の `workflow_context` は無視されます。

```shell
# ワーカー作成時にコンテキスト変数を固定設定
$ ./target/release/jobworkerp-client worker create \
    --name "MyWorkflow" \
    --runner-name WORKFLOW \
    --response-type DIRECT \
    --settings '{"workflow_url":"./my-workflow.yaml", "workflow_context":"{\"api_url\":\"https://api.example.com\",\"slack_token\":\"xoxb-xxx\"}"}'
```

**ジョブ引数（WorkflowRunArgs）で指定する場合：**

ランナー設定に `workflow_context` が未設定の場合のみ有効です。

```shell
# 実行時にコンテキスト変数を渡す
$ ./target/release/jobworkerp-client job enqueue \
    --worker "MyWorkflow" \
    --args '{"input":"/home", "workflow_context":"{\"api_url\":\"https://api.example.com\",\"max_retries\":3}"}'
```

#### 参照方法

jq構文では `$変数名`、Liquidテンプレート構文では `{{ 変数名 }}` で参照できます：

```yaml
do:
  - callApi:
      run:
        function:
          runnerName: HTTP_REQUEST
          arguments:
            # jq構文: $変数名 で参照
            url: ${"$api_url" + "/data"}
            maxRetries: ${$max_retries}
  - notify:
      run:
        function:
          runnerName: COMMAND
          arguments:
            # Liquidテンプレート構文: {{ 変数名 }} で参照
            command: "echo"
            args: ["$${API URL is {{ api_url }}}"]
```

#### export.asによるコンテキスト変数の追加

タスクの `export.as` を使うと、タスクの出力をコンテキスト変数として保存し、後続タスクで参照できます：

```yaml
do:
  - checkStatus:
      run:
        function:
          runnerName: COMMAND
          arguments:
            command: "test"
            args: ["-d", "/data"]
            treat_nonzero_as_error: false
      export:
        as:
          dir_exists: "${.exitCode == 0}"
  - processIfExists:
      if: "${$dir_exists == true}"
      run:
        # ...
```

## スキーマ定義

- [Serverless Workflow DSL仕様 v1.0.0](https://github.com/serverlessworkflow/specification/blob/v1.0.0/dsl.md) - ベースとなる公式DSL仕様
- [Serverless Workflow DSLリファレンス v1.0.0](https://github.com/serverlessworkflow/specification/blob/v1.0.0/dsl-reference.md) - 公式DSLの各タスク・プロパティの詳細リファレンス
- [jobworkerp-rs拡張スキーマ (runner/schema/workflow.yaml)](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/runner/schema/workflow.yaml) - jobworkerp-rs固有の拡張を含むスキーマ定義

## エージェントSkillによるワークフロー作成

[Claude Code](https://docs.anthropic.com/en/docs/claude-code)に[jobworkerp-workflow-plugin](https://github.com/jobworkerp-rs/jobworkerp-workflow-plugin)をインストールすると、`/jobworkerp-workflow` Skillが利用可能になります。このSkillを使うことで、自然言語でワークフローの要件を記述するだけで、jobworkerp-rs Custom Serverless Workflow DSL v1.0.0に準拠したYAML定義を自動生成できます。

Skillはfunction/runner/workerタスク定義、各ランナーの設定・引数スキーマ、jq/Liquid変数展開、フロー制御（for, switch, fork）、エラーハンドリング（try-catch）等のDSL仕様を参照して正確なワークフローを生成します。

セットアップ手順は [jobworkerp-workflow-plugin リポジトリ](https://github.com/jobworkerp-rs/jobworkerp-workflow-plugin) を参照してください。

## 公式Serverless Workflow仕様との違い

jobworkerp-rsのワークフローDSLは、公式仕様のロジック構築部分を採用していますが、エコシステムや運用機能には準拠していません。

**採用している部分（フロー制御・ロジック構築）:**
- タスク定義: `do`, `for`, `fork`, `switch`, `try`, `set`, `raise`, `wait`, `run`（拡張あり）
- `run`タスク: 公式仕様の`run.script`（language, code/source, arguments, environment）と同様の構造を採用
- ランタイム式: jq構文（`${}`）によるデータ変換（加えてLiquidテンプレート構文（`$${}`）を独自拡張）
- フロー制御: `then`プロパティによるフロー制御（`continue`, `exit`, `end`, `wait` ディレクティブ）
- 入出力変換: `input.from`, `output.as`, `export.as`

**採用していない部分:**
- **スケジュール・イベント駆動**: 公式仕様の`schedule`（cron, interval）、`listen`/`emit`（CloudEventsベースのイベント待機・発行）は未対応。ジョブのスケジューリングはjobworkerp-rsの[定期実行ジョブ機能](operations.md)で実現します
- **外部サービス呼び出し（call task）**: 公式仕様ではHTTP, gRPC, AsyncAPI, OpenAPIの各プロトコルを`call`タスクで標準化していますが、jobworkerp-rsでは`run`タスクのfunction/runner/workerを通じて、組み込みRunner（HTTP_REQUEST, GRPC等）やMCPサーバー、プラグインとして実行します
- **run.container / run.shell**: 公式仕様のコンテナ実行（`run.container`）やシェルコマンド実行（`run.shell`）は未対応。それぞれDOCKERランナー、COMMANDランナーを`run.function`/`run.runner`経由で利用することで同等の機能を実現します
- **run.workflow**: 公式仕様のネストワークフロー実行（`run.workflow`）は未対応。WORKFLOWランナーを`run.function`/`run.runner`経由で利用します
- **await / return**: 公式仕様の`run`タスクにおける`await`（完了待機）、`return`（stdout/stderr/code/all/none）はスキーマ定義には存在しますが、実行コードでは未実装です
- **認証・シークレット管理**: `use.authentications`、`$secrets`変数は未対応。認証情報はRunner設定や環境変数で管理します
- **カタログ・拡張**: `use.catalogs`（再利用可能コンポーネントの外部コレクション）、`use.extensions`（タスク前後の処理注入）、`use.functions`（外部関数定義）は未対応
- **ライフサイクルイベント**: CloudEventsによるワークフロー/タスクの状態変更通知は未対応。代わりにgRPCストリーミングでリアルタイム監視が可能です

**jobworkerp-rs独自の拡張:**
- `run.function` / `run.runner` / `run.worker`: jobworkerp-rsのジョブ実行基盤をワークフローから統一的に利用（組み込みRunner, MCPサーバー, プラグイン等）。公式仕様の`call`タスク、`run.container`、`run.shell`、`run.workflow`の機能をこれらで代替します
- `useStreaming`によるストリーミング実行（LLM等の長時間タスクの進捗監視）
- `checkpoint`によるチェックポイント・リスタート機能
- Liquidテンプレート構文（`$${}`）による文字列テンプレート

## 関連ドキュメント

- [ストリーミング](streaming.md) - ワークフローステップのストリーミング実行
