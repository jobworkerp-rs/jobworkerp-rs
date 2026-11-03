# Workflow Runner

## 概要

Workflow Runnerは、定義された順序で複数のジョブを実行したり、再利用可能なワークフローを実行したりするための機能です。[Open Workflow](https://open-workflow-specification.org/) ([DSL仕様 v1.0.0](https://github.com/open-workflow-specification/specification/blob/v1.0.0/dsl.md))のワークフローロジック構築方法（タスクの順序実行、分岐、ループ、エラーハンドリング等のフロー制御とランタイム式）に準拠しつつ、jobworkerp-rs独自のジョブ実行基盤に統合した拡張DSLです。([jobworkerp-rs拡張スキーマ](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/runner/schema/workflow.yaml))

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
| `workflow_context` | string (省略可) | コンテキスト変数（JSONオブジェクト文字列）。実行時引数の`workflow_context`とマージされ、キーが重複した場合は設定側が優先される |

> `workflow_url`と`workflow_data`は排他的です（`oneof workflow_source`）。ジョブ引数でワークフローソースを指定する場合、設定は省略可能です。

### ジョブ引数

#### `run`メソッド用（WorkflowRunArgs）

| フィールド | 型 | 説明 |
|-----------|------|------|
| `workflow_url` | string (oneof, 省略可) | URL/パスでワークフローソースを上書き |
| `workflow_data` | string (oneof, 省略可) | インライン定義でワークフローソースを上書き |
| `input` | string | ワークフローコンテキストへの入力（JSONまたはプレーン文字列） |
| `workflow_context` | string (省略可) | 追加のコンテキスト情報（JSONオブジェクト）。ランナー設定側の`workflow_context`とマージされ、キーが重複した場合は設定側が優先される |
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

root の `input` セクションは任意です。省略した場合、jobworkerp-rs は入力
schema 検証と `input.from` 変換を行わず、実行時に渡された input をそのまま
ワークフロータスクへ渡します。

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

> **注意**: `fromjson` は JSON 値のストリームをパースします。Markdown フェンス（`` ```json ... ``` ``）や先頭の無効トークンは JSON ではないため、式は実行時エラーになります。フェンス付きの LLM 出力をパースする前に、フェンスを取り除いてください。

### Raise タスクのエラー

`raise.error` は文字列または Error オブジェクトを受け取ります。文字列は jq/Liquid 式として評価され、評価結果が文字列なら既定の runtime error（status `500`）の `detail` として送出されます。評価結果が Error オブジェクトなら、その Error をそのまま送出できます。これにより `try.catch.as` で捕捉したエラーを再送出できます。

```yaml
catch:
  as: error
  do:
    - rethrow:
        raise:
          error: "${ $error }"
```

インライン Error の `title` と `detail` もランタイム式を評価します。いずれも評価結果は文字列でなければなりません。

```yaml
raise:
  error:
    type: https://example.invalid/errors/upstream
    status: 502
    title: '$${{{ input.service }}}'
    detail: '${ "requestId=" + $error.detail.requestId }'
```

### エラーの種類

ワークフローエンジンが送出するエラーは、Open Workflow Specification の標準エラータイプ `https://open-workflow-specification.org/spec/1.0.0/errors/<type>` を使います。

| type | status | 送出される場面 |
|---|---|---|
| `configuration` | 400 | 定義や配備がそのままでは実行できない場合（解決できない timeout 参照、call の設定の不備、未対応の script 言語やオプション、`run.script` に必要な runner がない、`for` の中の `then: wait`、子ジョブの runner・worker・メソッドが存在しないなど） |
| `validation` | 400 | ワークフローやタスクの入力が `input.schema` の検証に失敗した場合、`run.script` の引数名が識別子として不正な場合、または子ジョブの引数が runner の引数スキーマに合わない場合 |
| `expression` | 400 | jq / Liquid の式の評価に失敗した場合、または `raise.error` の評価結果の型が違う場合 |
| `authorization` | 403 | call が、`use.secrets` に宣言されていない secret、または値が設定されていない secret を参照した場合 |
| `timeout` | 408 | タスクまたはワークフローが `timeout` を超えた場合（[タスクのタイムアウト](#タスクのタイムアウト)を参照）、子ジョブが runner 内部の時間制限を超えた場合、または `call: grpc` の応答が `DEADLINE_EXCEEDED` の場合 |
| `communication` | 500（call の応答 status による場合はその値） | `call: http` / `call: grpc` の呼び出しに失敗した場合、`call: http` の応答 status が受け付ける範囲外の場合、`call: grpc` の応答が非 OK status の場合、`run.script` の外部ソースを取得できない場合、または子ジョブが下流サービスとの通信に失敗した場合 |
| `runtime` | 500 | run タスクが起動した子ジョブが、ほかの種類に当たらない理由で失敗した場合、その他ワークフローの実行中に失敗した場合 |

`authentication`（401）も標準エラータイプですが、エンジンは送出しません。`raise` タスクで投げたエラーは、定義した `type` と `status` のままです。`call: http` は、`redirect` が false（デフォルト）なら 200–299、true なら 200–399 の範囲外の status の応答で `communication` エラーになります。リダイレクトは自動で追従し、最後の応答の status で判定します。エラーの `status` は応答の status、`title` は `HTTP 404 Not Found` の形式、`detail` は応答本文（64 KiB まで。`$secrets` で参照した secret の値、`authentication` で使った認証情報、endpoint のユーザー情報はマスクされます）です。`call: grpc` は非 OK status の応答で `communication` エラーになり、`status` は gRPC の code に対応する HTTP status（grpc-gateway と同じ対応。例: `NOT_FOUND`→404、`UNAVAILABLE`→503）、`title` は `gRPC NOT_FOUND` の形式、`detail` は status message（同様に secret と認証情報はマスクされます）です。接続失敗など、call の実行自体が失敗したときのエラーの `title` / `detail` でも同じ値がマスクされます。ジョブの結果やストリームでクライアントに返るエラーの `detail` は 4096 バイトまでに切り詰められます。HTTP_REQUEST / GRPC runner を `run.runner` で直接使う場合は、従来どおり応答を結果データとして返します。

#### 子ジョブの失敗

run タスクと call タスクは、処理を子ジョブとして worker で実行します。子ジョブの失敗は、失敗した段階と原因から次の type になります。

| 失敗 | type |
|---|---|
| runner・worker・`using` のメソッドが存在しない、runner settings がスキーマに合わない、ストリーミングの可否が worker の定義と合わない | `configuration` |
| 引数が runner の引数スキーマに合わない、runner が実行時に引数を拒否した | `validation` |
| runner 内部の時間制限（GRPC runner の `timeout` など）を超えた、ストリーミングの run で結果を待つ間に時間切れになった | `timeout` |
| 下流サービスとの通信に失敗した、LLM プロバイダのストリームが中断した | `communication` |
| 子ワークフロー（`run.workflow`、WORKFLOW runner）が失敗した | 子ワークフローで発生したエラーの type と status |
| それ以外（コマンドの失敗、ジョブの取り消し、投入や結果の取得の失敗など） | run タスクは `runtime`、call タスクは `communication` |

子ワークフローが失敗したときは、子で発生したエラー（タスクの timeout、入力の検証の失敗、`raise` したユーザー定義のエラーなど）の type、status、title をそのまま親のエラーにします。`instance` は親のタスクの位置になり、子の中の位置は `detail` に入ります。孫ワークフローのエラーも同じように伝わります。

古い worker が実行した子ジョブなど、原因が記録されていない失敗は、表の最後の行と同じ扱いになります。

`catch.errors.with.type` は、URI テンプレートとしてエラーの `type` と照合します。

- `{...}` を含まない値は、type と完全に一致する必要があります。大文字と小文字は区別します。
- `{name}` は任意の文字列に一致します。`https://open-workflow-specification.org/spec/1.0.0/errors/{kind}` と書くと、すべての標準エラーを捕捉できます。
- `https://serverlessworkflow.io/spec/`（仕様の 1.0.0 版が公開されたドメイン）と `https://open-workflow-specification.org/spec/` は同じものとして照合します。
- スキームのない値（例: `badargument`）は単なる文字列として照合し、warn ログを出します。リテラルのエラー type は絶対 URI でなければならないためです。

以前の版は `http-error://...` 形式の type（例: `http-error://serviceunavailable`）を送出し、スキームのないフィルタに `http-error://` を補っていました。どちらも廃止したので、このようなフィルタは標準エラータイプに書き換えてください。一部の status も変わっています（例: run タスクの失敗は `503` ではなく `500`）。

また以前の版では、子ジョブの失敗は原因にかかわらず run タスクで `runtime`、call タスクで `communication` でした。子ジョブの失敗をまとめて捕捉するには、`.../errors/{kind}` のようなテンプレートを使うか、type のフィルタを外してください。

### タスクのタイムアウト

タスクの `timeout`（およびワークフロー全体の `timeout`）は任意指定です。指定しないタスクにタイムアウトはありません。ただし `WORKFLOW_TASK_DEFAULT_TIMEOUT_SEC` を設定した場合は、`timeout` 未指定の run/call タスクだけにその値が適用されます。do/try/for/fork などのタスクには適用されないため、外側のタスクの `timeout` を内側より長く調整する必要はありません。ワークフロー文書直下の `timeout` は暗黙のルートタスクにだけ適用され、内側のタスクの既定値にはなりません（期限に達するとワークフロー全体が中断されます）。

タイムアウトしたタスクの内側で実行中の子ジョブは取り消されます。発生するエラーは、`status` が `408` の標準エラー `timeout` です（[エラーの種類](#エラーの種類)を参照）。

```yaml
- guarded:
    try:
      - slow:
          timeout:
            after:
              minutes: 10
          run:
            function:
              runnerName: COMMAND
              arguments:
                command: ./long-job.sh
    catch:
      errors:
        with:
          type: https://open-workflow-specification.org/spec/1.0.0/errors/timeout
      do:
        - fallback:
            set:
              timedOut: true
```

### 子ジョブの取り消し

ワークフローが実行中の子ジョブ（run/call タスクが発行したジョブ）を取り消すのは、次のように処理を打ち切ると決めた時点だけです。

- ワークフロージョブの明示的な取り消し（JobService の Delete など）
- ワークフロージョブ自体の job timeout
- タスクのタイムアウト（タイムアウトしたタスクの内側の子ジョブだけが対象。fork の他の枝や並列 for の他の item は対象外）
- `inParallel: true` の for で、ある item が失敗して `onError: break` により打ち切られたとき（他の item の子ジョブ）。`onError: continue` では他の item に影響しません
- `fork` の `compete: true` で勝者が決まったとき（負けた枝の子ジョブ）

ストリームの受信側が切断されただけでは子ジョブは取り消されません。結果は後から ListenStream などで受け取り直せます。

### 環境変数の参照

jq構文では、jaqクレートの組み込み `env` 関数を通じてプロセスの環境変数を参照できます：

```yaml
# jq構文での環境変数参照
apiKey: ${env.API_KEY}
```

> **注意**: 環境変数の参照はjq構文（`${...}`）でのみ利用可能です。Liquidテンプレート構文（`$${...}`）では環境変数を直接参照できません。

### コンテキスト変数

ワークフロー実行時に `input` とは別に、`workflow_context` パラメータでグローバル変数を渡すことができます。渡されたJSONオブジェクトの各キーがワークフロー全体でトップレベル変数として参照可能になります。

#### 入力方法

**ランナー設定（WorkflowRunnerSettings）で指定する場合：**

ワーカー作成時に `runner_settings` の `workflow_context` にJSON文字列を設定します。実行時引数の `workflow_context` とマージされ、キーが重複した場合は設定側の値が優先されます。

```shell
# ワーカー作成時にコンテキスト変数を固定設定
$ ./target/release/jobworkerp-client worker create \
    --name "MyWorkflow" \
    --runner-name WORKFLOW \
    --response-type DIRECT \
    --settings '{"workflow_url":"./my-workflow.yaml", "workflow_context":"{\"api_url\":\"https://api.example.com\",\"tenant\":\"example\"}"}'
```

**ジョブ引数（WorkflowRunArgs）で指定する場合：**

ランナー設定の `workflow_context` とマージされます。両方に同じキーがある場合は設定側の値が使われます。

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

- [Open Workflow DSL仕様 v1.0.0](https://github.com/open-workflow-specification/specification/blob/v1.0.0/dsl.md) - ベースとなる公式DSL仕様
- [Open Workflow DSLリファレンス v1.0.0](https://github.com/open-workflow-specification/specification/blob/v1.0.0/dsl-reference.md) - 公式DSLの各タスク・プロパティの詳細リファレンス
- [Open Workflow Schema v1.0.0](https://open-workflow-specification.org/schemas/1.0.0/workflow.json) - 公式JSON Schema
- [jobworkerp-rs拡張スキーマ (runner/schema/workflow.yaml)](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/runner/schema/workflow.yaml) - jobworkerp-rs固有の拡張を含むスキーマ定義

## エージェントSkillによるワークフロー作成

[Claude Code](https://docs.anthropic.com/en/docs/claude-code)に[jobworkerp-workflow-plugin](https://github.com/jobworkerp-rs/jobworkerp-workflow-plugin)をインストールすると、`/jobworkerp-workflow` Skillが利用可能になります。このSkillを使うことで、自然言語でワークフローの要件を記述するだけで、jobworkerp-rs Custom Open Workflow DSL v1.0.0に準拠したYAML定義を自動生成できます。

Skillはfunction/runner/workerタスク定義、各ランナーの設定・引数スキーマ、jq/Liquid変数展開、フロー制御（for, switch, fork）、エラーハンドリング（try-catch）等のDSL仕様を参照して正確なワークフローを生成します。

セットアップ手順は [jobworkerp-workflow-plugin リポジトリ](https://github.com/jobworkerp-rs/jobworkerp-workflow-plugin) を参照してください。

## 公式Open Workflow仕様との違い

jobworkerp-rsのワークフローDSLは、公式仕様のロジック構築部分を採用していますが、エコシステムや運用機能には準拠していません。

**採用している部分（フロー制御・ロジック構築）:**
- タスク定義: `do`, `for`, `fork`, `switch`, `try`, `set`, `raise`, `wait`, `run`（拡張あり）
- `run`タスク: 公式仕様の`run.script`（language, code/source, arguments, environment）と同様の構造、および`run.shell` / `run.container` / jobworkerp独自のsource指定形式を持つ`run.workflow`を採用
- `call`タスク: `call: http` の基本的なHTTP呼び出し（method, endpoint, headers, query, body, output）を採用。endpoint の `authentication`（bearer / basic）にも対応。`call: grpc` の unary 呼び出し（service の name/host/port、method、arguments）にも対応（サーバーリフレクション利用、service の `authentication` は bearer のみ）
- ランタイム式: jq構文（`${}`）によるデータ変換（加えてLiquidテンプレート構文（`$${}`）を独自拡張）
- フロー制御: `then`プロパティによるフロー制御（`continue`, `exit`, `end`, `wait` ディレクティブ）
- 入出力変換: `input.from`, `output.as`, `export.as`

`run.shell` / `run.container` / `run.workflow` は、公式タスク定義そのものではありません：

- `run.shell`: `withMemoryMonitoring`、`treatNonzeroAsError`、`successExitCodes` は jobworkerp 拡張です。監視と、終了コードをタスク失敗として扱う条件に影響します。
- `run.container`: `timeoutSec`、`treatNonzeroAsError`、`successExitCodes` は jobworkerp 拡張です。ジョブタイムアウトと、終了コードをタスク失敗として扱う条件に影響します。
- `await` は **すべての** `run.*` インスタンス（`run.runner` / `run.function` / `run.worker` / `run.script` / `run.workflow` / `run.shell` / `run.container`）で利用できる共通オプションです。`await: false` は完了を待たずジョブを fire-and-forget で投入し、現在のタスク入力をタスク出力にして続行します。デフォルトは `await: true` です。（`useStreaming: true` の場合、ストリーミングは runner result の収集を前提とするため `await: false` は拒否されます。）
- `return` は process result `{code, stdout, stderr}` を生成する process タスク（`run.script` / `run.shell` / `run.container`）で利用できます。runner / function / worker / workflow は runner 定義の出力型を持つため `return` を拒否します。`return` は `stdout`、`stderr`、`code`、`all`、`none` を受け付け、デフォルトは `stdout`（**stdout の生文字列**）、`return: all` は `{code, stdout, stderr}` を出力します。`run.script` のデフォルト `stdout` は stdout のテキストをそのまま返します。Open Workflow v1.0.0 の process result 契約に合わせ、**JSON への自動パースは行いません**。構造化出力を扱うには、スクリプトで JSON を出力して返り値の文字列をパースするか、`return: all` を使用してください。
  - 終了コードの扱いは process タスク間で異なります。`run.shell` / `run.container` は `treatNonzeroAsError` / `successExitCodes` 拡張を受け付け、デフォルトでは非ゼロ終了をタスク失敗にしません。そのため `return: code` や `return: all` の `code` フィールドで非ゼロの終了コードを取得できます。`run.script` にはこの手段がなく、非ゼロ終了は `return` 適用前に必ずタスク失敗となるため、script の `return: code` / `return: all` で観測できる `code` は成功時（`0`）のみです。
- `run.script`: streaming は未対応です（PYTHON_COMMAND runner にストリーミング出力がないため）。script タスクは `useStreaming: false`（デフォルト）で実行する必要があります。タスク自身の `useStreaming` が false であれば、streaming 実行中のワークフロー内でも実行できます。
- `run.workflow`: `workflowData` / `workflowUrl` はネストしたワークフローの source を指定する jobworkerp 拡張です。公式の workflow process 参照形式はここでは未実装です。親 workflow context は暗黙的に引き継がれ、`workflowContext` は DSL field として定義しません。
- `call: http`: `output: response` は公式仕様の HTTP Response の形 `{ request: { method, uri, headers }, statusCode, headers, content }` を出力します。`request` は実際に送ったリクエストで、`Authorization` / `Proxy-Authorization` / `Cookie` ヘッダーの値、URI のユーザー情報（`user:password@`）、`$secrets` で参照した secret の値（URI 内でパーセントエンコード・フォームエンコードされたものを含む）は `***` に置き換えられます。ワークフロー変数など `$secrets` 以外から渡した値はマスクされないため、認証情報は `use.secrets` で宣言して `$secrets` から参照してください。また、secret を式で加工した値（例: `${ $secrets.KEY | ascii_upcase }`）は元の secret と一致しないためマスクされません。ただし `authentication` で送る認証情報は、加工後の実際に送る値でマスクされます。応答の `headers` は同名のヘッダーを `, ` で連結した文字列になります（`Set-Cookie` は連結すると値の区切りが曖昧になる点に注意してください）。
- `call: grpc`: 公式の `service`（name / host / port / authentication）、`method`、`arguments` を採用した unary 呼び出しです。jobworkerp の GRPC runner へアダプター変換され、`arguments` は JSON ボディとしてサーバーリフレクション経由で protobuf に変換されます（`use_reflection` 有効が前提）。成功時の出力は `{ code, body, metadata, message? }` に整形され、`body` はレスポンスを JSON パースした構造です。非 OK status はエラーになります（[エラーの種類](#エラーの種類)を参照）。GRPC runner 自体は proto descriptor 指定に対応していますが、workflow executor の公式 `proto`（externalResource）受け入れ解除は後続作業のため、現時点の `call: grpc` では実行時に拒否されます。service の `authentication` は bearer のみ対応で、basic は拒否されます（GRPC runner の単一 `auth_token` フィールドでは表現できないため）。server streaming は未対応です。

例：

```yaml
do:
  - echo:
      run:
        shell:
          command: echo
          args: ["hello"]
  - boxed:
      run:
        container:
          image: alpine:3.20
          command: ["echo", "hello"]
  - child:
      run:
        workflow:
          workflowUrl: file:///workflows/child.yaml
          input:
            value: 1
```

**採用していない部分:**
- **スケジュール・イベント駆動**: 公式仕様の`schedule`（cron, interval）、`listen`/`emit`（CloudEventsベースのイベント待機・発行）は未対応。ジョブのスケジューリングはjobworkerp-rsの[定期実行ジョブ機能](operations.md)で実現します
- **外部サービス呼び出し（call task）**: `call: http`（endpoint の `authentication` は `bearer` / `basic` scheme に対応）と `call: grpc` の unary 呼び出し（service の `authentication` は `bearer` のみ）に対応しています。`call: grpc` の `proto`（externalResource、リフレクションを使うため）・basic 認証・server streaming、AsyncAPI、OpenAPI、named function call、HTTP の `digest` / `oauth2` / `oidc` / `certificate` scheme、HTTP `output: raw` は未対応です
- **await / return の適用範囲**: `await` はすべての `run.*` インスタンスで利用できます（`await: false` = fire-and-forget）。`return` は process タスク（script / shell / container）で利用でき、runner / function / worker / workflow は拒否します。`useStreaming: true` の場合、すべてのインスタンスで `await: false` は拒否されます（`run.script` は `useStreaming: true` 自体が未対応）
- **認証・シークレット管理**: `call: http` の endpoint 認証向けに `use.authentications`（`authentication: { use: <name> }` で参照する bearer/basic の名前付きポリシー）、`use.secrets`（secret 名の宣言）、公式の `secretBasedAuthenticationPolicy`（`bearer: { use: <name> }` / `basic: { use: <name> }`）、`$secrets` 変数に対応しています。secret 値は workflow 定義に平文で書きません。名前を `use.secrets` で宣言し、各値は環境変数 `WORKFLOW_SECRET_<大文字スネーク名>` で供給します（例 `github-token` → `WORKFLOW_SECRET_GITHUB_TOKEN`）。bearer の env 値は token 文字列（または `{"token": "..."}` JSON）、basic は `{"username": "...", "password": "..."}` JSON です。scheme 値への secret バインドは3通り: (1) 公式の `bearer: { use: <name> }` / `basic: { use: <name> }` 参照、(2) ランタイム式 `bearer: { token: "${ $secrets.<name> }" }`、(3) リテラルのインライン値（認証情報では非推奨）。`{ use }` と `$secrets` のどちらも宣言済みの名前のみ解決し、未宣言の参照は `authorization` エラーで環境変数を読みません（jq の `null`・Liquid の空文字へ静かに展開されることはありません）。`$secrets` は `call` タスクの評価時にのみ注入され、workflow context へは書き戻されません。`secrets` は予約名であり、ワークフロー入力に `{"secrets": {...}}` が含まれていても式評価時の `$secrets`（認証情報）を上書きできません。secret 由来の `Authorization` ヘッダー値は debug ログでマスクされます。`digest` / `oauth2` / `oidc` / `certificate` scheme は未対応です。（LLM の API キーなどその他の認証情報は引き続き Runner 設定や環境変数で管理します。）
- **カタログ・拡張**: `use.catalogs`（再利用可能コンポーネントの外部コレクション）、`use.extensions`（タスク前後の処理注入）、`use.functions`（外部関数定義）は未対応
- **ライフサイクルイベント**: CloudEventsによるワークフロー/タスクの状態変更通知は未対応。代わりにgRPCストリーミングでリアルタイム監視が可能です

**jobworkerp-rs独自の拡張:**
- `run.function` / `run.runner` / `run.worker`: jobworkerp-rsのジョブ実行基盤をワークフローから統一的に利用（組み込みRunner, MCPサーバー, プラグイン等）
- `useStreaming`によるストリーミング実行（LLM等の長時間タスクの進捗監視）
- `checkpoint`によるチェックポイント・リスタート機能
- Liquidテンプレート構文（`$${}`）による文字列テンプレート
- `catch.errors.with.type` の URI テンプレート照合（`{name}` のプレースホルダー）

## 関連ドキュメント

- [ストリーミング](streaming.md) - ワークフローステップのストリーミング実行
