# Workflow Runner

[English ver.](WORKFLOW.md)

## 概要

Workflow Runnerは、定義された順序で複数のジョブを実行したり、再利用可能なワークフローを実行したりするための機能です。この機能は[Serverless Workflow](https://serverlessworkflow.io/) (v1.0.0)をベースにしており、機能の削除およびjobworkerp-rs独自の拡張機能(run taskのrunner, worker)が追加されています。([詳細(schema)](runner/schema/workflow.yaml))

## WORKFLOW ランナー（統合版）

`WORKFLOW`ランナーはマルチメソッドランナーとして、jobworkerp-rsを通じてワークフロー機能を提供します。`using`パラメータで実行メソッドを指定します。

### メソッド

#### run（デフォルト）

ワークフロー定義を実行します。`using`が指定されない場合のデフォルトメソッドです。

- ワークフローは`worker.runner_settings`に事前設定するか、ジョブ引数で実行時に指定可能
- 両方指定された場合、ジョブ引数が設定より優先される
- jq構文（`${}`）とLiquidテンプレート構文（`$${}`）の両方を使用した動的変数展開をサポート
- リアルタイム監視のためのストリーミング実行をサポート
- チェックポイントベースの再開をサポート

#### create

ワークフロー定義から新しいWORKFLOWランナーワーカーを作成します。

- ワークフロー定義を再利用可能なワーカーとして登録
- 作成されたワーカーは`run`メソッドで異なる入力で繰り返し実行可能

### ランナー設定（WorkflowRunnerSettings）

`run`メソッド用に`worker.runner_settings`で設定するフィールド：

| フィールド | 型 | 説明 |
|-----------|------|------|
| `workflow_url` | string (oneof) | ワークフロー定義ファイルへのパスまたはURL（JSON/YAML） |
| `workflow_data` | string (oneof) | インラインのワークフロー定義（JSON/YAML文字列） |

> `workflow_url`と`workflow_data`は排他的です（`oneof workflow_source`）。ジョブ引数でワークフローソースを指定する場合、設定は省略可能です。

### ジョブ引数

#### `run`メソッド用（WorkflowRunArgs）

| フィールド | 型 | 説明 |
|-----------|------|------|
| `workflow_url` | string (oneof, 省略可) | URL/パスでワークフローソースを上書き |
| `workflow_data` | string (oneof, 省略可) | インライン定義でワークフローソースを上書き |
| `input` | string | ワークフローコンテキストへの入力（JSONまたはプレーン文字列） |
| `workflow_context` | string (省略可) | 追加のコンテキスト情報（JSON） |
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
| `status` | WorkflowStatus | 実行ステータス: `Completed`, `Faulted`, `Cancelled`, `Running`, `Waiting` |
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
  id: 1
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
        runner:
          name: COMMAND
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
              runner:
                name: COMMAND
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

## スキーマ定義

ワークフローのスキーマは [runner/schema/workflow.yaml](runner/schema/workflow.yaml) で定義されています。

## 非推奨ランナー

以下のランナーは非推奨です。代わりに`WORKFLOW`ランナーの`using`パラメータを使用してください：

| 非推奨ランナー | 代替 |
|---------------|------|
| `INLINE_WORKFLOW` (ID: 65535) | `WORKFLOW` + `using: "run"` でワークフローソースをジョブ引数に指定 |
| `REUSABLE_WORKFLOW` (ID: 65532) | `WORKFLOW` + `using: "run"` でワークフローソースを設定に指定 |
| `CREATE_WORKFLOW` (ID: -1) | `WORKFLOW` + `using: "create"` |

## 関連ドキュメント

- [ストリーミング](STREAMING_ja.md) - ワークフローステップのストリーミング実行
