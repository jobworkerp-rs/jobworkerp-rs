# jobworkerp-rs

## 概要

jobworkerp-rs は、Rustで実装されたスケーラブルなジョブワーカーシステム。
ジョブワーカーシステムは、CPU負荷やI/O負荷の高いタスクを非同期に処理するために利用する。
GRPCをつかって処理内容となる[Worker](proto/protobuf/jobworkerp/service/worker.proto)の定義・処理実行のための[Job](proto/protobuf/jobworkerp/service/job.proto)の登録、実行結果の取得などを実行できる。
プラグイン形式で処理を拡張できる。

### 主な機能

- ジョブキューとして利用できるストレージ: 状況に応じてRedis、RDB（MySQLまたはSQLite）を使いわける
- 3種類のジョブ実行結果の取得方法: 直接取得（DIRECT）、後から取得（LISTEN_AFTER）、結果取得しない（NONE）
- ジョブ実行チャネルの設定とチャネル毎の並列実行数の設定
  - 例えば、GPUチャネルでは並列度1で実行、通常チャネルでは並列度4で実行などの設定ができる
- 指定時刻実行、一定間隔での定期実行
- ジョブ実行失敗時のリトライ機能: リトライ回数や間隔の設定（Exponential backoff 他）
- プラグインによる実行ジョブ内容（Runner）の拡張


<!-- START doctoc generated TOC please keep comment here to allow auto update -->
<!-- DON'T EDIT THIS SECTION, INSTEAD RE-RUN doctoc TO UPDATE -->

- [Command Examples](#command-examples)
  - [ビルド、起動](#%E3%83%93%E3%83%AB%E3%83%89%E8%B5%B7%E5%8B%95)
    - [docker imageでの起動例](#docker-image%E3%81%A7%E3%81%AE%E8%B5%B7%E5%8B%95%E4%BE%8B)
  - [jobworkerp-client による実行例](#jobworkerp-client-%E3%81%AB%E3%82%88%E3%82%8B%E5%AE%9F%E8%A1%8C%E4%BE%8B)
- [jobworkerp-workerの機能詳細](#jobworkerp-worker%E3%81%AE%E6%A9%9F%E8%83%BD%E8%A9%B3%E7%B4%B0)
  - [worker.schema_idの組み込み機能](#workerschema_id%E3%81%AE%E7%B5%84%E3%81%BF%E8%BE%BC%E3%81%BF%E6%A9%9F%E8%83%BD)
  - [ジョブキュー種別](#%E3%82%B8%E3%83%A7%E3%83%96%E3%82%AD%E3%83%A5%E3%83%BC%E7%A8%AE%E5%88%A5)
  - [結果の格納 (worker.store_success、worker.store_failure)](#%E7%B5%90%E6%9E%9C%E3%81%AE%E6%A0%BC%E7%B4%8D-workerstore_successworkerstore_failure)
  - [結果の取得方法 (worker.response_type)](#%E7%B5%90%E6%9E%9C%E3%81%AE%E5%8F%96%E5%BE%97%E6%96%B9%E6%B3%95-workerresponse_type)
- [その他詳細](#%E3%81%9D%E3%81%AE%E4%BB%96%E8%A9%B3%E7%B4%B0)
  - [worker定義](#worker%E5%AE%9A%E7%BE%A9)
  - [RDBの定義](#rdb%E3%81%AE%E5%AE%9A%E7%BE%A9)
  - [その他の環境変数](#%E3%81%9D%E3%81%AE%E4%BB%96%E3%81%AE%E7%92%B0%E5%A2%83%E5%A4%89%E6%95%B0)
- [プラグインについて](#%E3%83%97%E3%83%A9%E3%82%B0%E3%82%A4%E3%83%B3%E3%81%AB%E3%81%A4%E3%81%84%E3%81%A6)
  - [各種エラーコードについて](#%E5%90%84%E7%A8%AE%E3%82%A8%E3%83%A9%E3%83%BC%E3%82%B3%E3%83%BC%E3%83%89%E3%81%AB%E3%81%A4%E3%81%84%E3%81%A6)
- [その他](#%E3%81%9D%E3%81%AE%E4%BB%96)

<!-- END doctoc generated TOC please keep comment here to allow auto update -->

## Command Examples

--------

### ビルド、起動

```shell

# prepare .env file
$ cp dot.env .env

# build release binaries (use mysql)
$ cargo build --release --features mysql

# build release binaries
$ cargo build --release

# Run the all-in-one server by release binary
$ ./target/release/all-in-one

# Run gRPC front server and worker by release binary
$ ./target/release/worker &
$ ./target/release/grpc-front &

```

#### docker imageでの起動例

- docker-commpose.yml, docker-compose-scalable.ymlを参照してください

### jobworkerp-client による実行例

[jobworkerp-client](https://github.com/jobworkerp-rs/jobworkerp-client-rs)をつかって以下のようにworkerの作成・取得、jobのenqueue、処理結果の取得が可能

(worker.operation, job.job_arg, job_result.outputのencode・decodeが不要であればgrpcurlでも実行可能。参考: [protoファイル](proto/protobuf/jobworkerp/service/))

setup:

```shell
# clone
$ git clone https://github.com/jobworkerp-rs/jobworkerp-client-rs
$ cd jobworkerp-client-rs

# build
$ cargo build --release

# run (show help)
$ ./target/release/jobworkerp-client

# list worker-schema (need launching jobworkerp-rs in localhost:9000(default))
$ ./target/release/jobworkerp-client worker-schema list
```

one shot job (with result: response-type DIRECT)

```shell
# create worker (specify schema id from worker-schema list)
1. $ ./target/release/jobworkerp-client worker create --name "GoogleRequest" --schema-id 2 --operation '{"base_url":"https://www.google.com/search"}' --response-type DIRECT

# enqueue job (ls . ..)
# specify worker_id value or worker name created by `worker create` (command 1. response)
2-1. $ ./target/release/jobworkerp-client job enqueue --worker 1 --arg '{"headers":[],"method":"GET","path":"/search","queries":[{"key":"q","value":"test"}]}'
2-2. $ ./target/release/jobworkerp-client job enqueue --worker "GoogleRequest" --arg '{"headers":[],"method":"GET","path":"/search","queries":[{"key":"q","value":"test"}]}'
```

one shot job (listen result after request: response-type LISTEN_AFTER)

```shell

# create shell command `sleep` worker (must specify store_success and store_failure to be true)
1. $ ./target/release/jobworkerp-client worker create --name "SleepWorker" --schema-id 1 --operation '{"name":"sleep"}' --response-type LISTEN_AFTER --store-success --store-failure

# enqueue job
# sleep 60 seconds
2. $ ./target/debug/jobworkerp-client job enqueue --worker 'SleepWorker' --arg '{"args":["60"]}'

# listen job (long polling with grpc)
# specify job_id created by `job enqueue` (command 2. response)
3. $ ./target/release/jobworkerp-client job-result listen --job-id <got job id above> --timeout 70000 --worker 'SleepWorker'
# (The response is returned as soon as the result is available, to all clients to listen. You can request repeatedly)
```


periodic job

```shell

# create periodic worker (repeat per 3 seconds)
1. $ ./target/release/jobworkerp-client worker create --name "PeriodicEchoWorker" --schema-id 1 --operation '{"name":"echo"}' --periodic 3000 --response-type NO_RESULT --store-success --store-failure

# enqueue job (echo Hello World !)
# start job at [epoch second] % 3 == 1, per 3 seconds by run_after_time (epoch milliseconds) (see info log of jobworkerp all-in-one execution)
# (If run_after_time is not specified, the command is executed repeatedly based on enqueue_time)
2. $ ./target/debug/jobworkerp-client job enqueue --worker 'PeriodicEchoWorker' --arg '{"args":["Hello", "World", "!"]}' --run-after-time 1000

# stop periodic job 
# specify job_id created by `job enqueue` (command 2. response)
3. $ ./target/debug/jobworkerp-client job delete --id <got job id above>

```

## jobworkerp-workerの機能詳細

### worker.schema_idの組み込み機能
worker_schemaに組み込み定義されている機能を以下に記載する。
各機能のworker.operation、job.argにはprotobufでそれぞれの機能に必要な値を設定する。protobuf定義はworker_schema.operation_proto, worker_schema.job_arg_protoから取得可能。

- COMMAND: command実行 ([ComamndRunner](infra/src/infra/runner/command.rs)): worker.operationに対象のコマンドを指定、job.argに引数を指定する
- HTTP_REQUEST: reqwestによるhttpリクエスト ([RequestRunner](infra/src/infra/runner/request.rs)): worker.operationにbase url、job.argにheaders、queries、method、body、pathを指定する。レスポンス本文を結果として受け取る
- GRPC_UNARY: gRPC unaryリクエスト ([GrpcUnaryRunner](infra/src/infra/runner/grpc_unary.rs)): worker.operationにjson形式でurlとpathを指定する (例: `{"url":"http://localhost:9000","path":"jobworkerp.service.WorkerService/FindList"}`)。job.argはrpc引数をprotobufエンコード(bytes)で指定する。レスポンスはprotobuf バイナリを受けとる。
- DOCKER: docker run実行 ([DockerRunner](infra/src/infra/runner/docker.rs)): worker.operationにFromImage (pullするイメージ)、Repo (レポジトリ)、Tag、Platform(`os[/arch[/variant]]`)などを指定、job.argsにImage(実行するイメージ名)とCmd(実行コマンドラインの配列)を指定する 
  - 環境変数 `DOCKER_GID`：/var/run/docker.sock に接続する権限をもったGIDを指定する。jobworkerpの実行プロセスはこのGIDを利用可能な権限が必要。
  - k8s pod上での起動は現在未テスト。(上記の制限からおそらくDockerOutsideOfDockerあるいはDockerInDockerが可能なdocker imageの設定が必要になる想定)。


### ジョブキュー種別

環境変数`STORAGE_TYPE`
- Standalone: 即時ジョブは memory(spmc channel) 、時刻指定ジョブなどはrdb(sqlite, mysql)に格納するためシングルインスタンスでの実行のみサポート
- Scalable: 即時ジョブは redis 、時刻指定ジョブなどはrdb(sqlite, mysql)に格納するためgrpc-front、workerをそれぞれ複数台で構成することができる
    - cargoでのビルド時に `--features mysql` を付けてビルドする必要がある

worker.queue_type
- NORMAL: 即時実行ジョブ(時刻指定のない通常のジョブ)はchannel (redis) に、定期実行や時刻指定ジョブはdbに格納
- WITH_BACKUP: 即時実行ジョブをchannelとrdbの両方に格納する(障害時にrdb内のジョブをリストアできる)
- FORCED_RDB: 即時実行ジョブもrdbのみに格納する (実行が遅くなることがある)

### 結果の格納 (worker.store_success、worker.store_failure)

- worker.store_success、worker.store_failureの指定により実行成功、失敗時にrdb(job_resultテーブル) に保存
- [JobResultService](proto/protobuf/jobworkerp/service/job_result.proto)で実行後に参照できる

### 結果の取得方法 (worker.response_type)

- 結果取得なし (NO_RESULT): (デフォルト値) Job IDがレスポンスで返される。結果を格納している場合はjobの終了後に [JobResultService/FindListByJobId](proto/protobuf/jobworkerp/service/job_result.proto) をつかって取得できる。
- 後で取得(LISTEN_AFTER): enqueue後、[job_result](proto/protobuf/jobworkerp/service/job_result.proto)サービスのListenを使って実行終了後すぐに結果を取得できる。(ロングポーリング)
  - 複数のクライアントがListenして全クライアントが同じ結果を得ることができます (Redis pubsubでの伝達)
- 直接取得(DIRECT): enqueueリクエストで実行完了まで待ち、そのレスポンスとして直接結果が得られる。(結果を格納していない場合はリクエストしたクライアントのみ結果を取得可能)

## その他詳細

- 特に単位を明記していない時間の項目の単位はミリ秒

### worker定義

- run_after_time: ジョブの実行時刻 (epoch time)
- timeout：タイムアウト時間
- worker.periodic_interval: 繰り返しジョブ実行 (1以上の指定)
- worker.retry_policy: job実行失敗時のリトライ方式(RetryType: CONSTANT、LINEAR、EXPONENTIAL)、最大回数(max_retry)、最大時間間隔(max_interval)などを指定
- worker.next_workers: ジョブの実行完了後にその結果を引数として別のworkerを実行 (worker.idをカンマ区切りで指定)
  - 結果の値をそのままjob_argとして指定して処理可能なworkerを指定する必要がある
- worker.use_static (テスト中): runnerプロセスを並列度の分だけstaticに確保することが可能 (実行runerをpoolingして初期化を都度行わない)

### RDBの定義

- [MySQL schema](infra/sql/mysql/002_worker.sql)
- [SQLite schema](infra/sql/sqlite/001_schema.sql)

(worker_schemaには組み込み機能としての固定レコードが存在する)

### その他の環境変数

  - 実行runner設定
    - `PLUGINS_RUNNER_DIR`: プラグイン格納ディレクトリ
    - `DOCKER_GID`: DockerグループID (DockerRunner用)
  - ジョブキューチャンネルと並列度
    - `WORKER_DEFAULT_CONCURRENCY`: デフォルトチャンネルの並列度
    - `WORKER_CHANNELS`: 追加ジョブキューチャンネルの名称(カンマ区切り)
    - `WORKER_CHANNEL_CONCURRENCIES`: 追加ジョブキューチャンネルの並列度(カンマ区切り、WORKER_CHANNELSに対応した値)
  - ログ設定 
    - `LOG_LEVEL`: ログレベル(trace, debug, info, warn, error)
    - `LOG_FILE_DIR`: ログ出力ディレクトリ
    - `LOG_USE_JSON`: ログ出力をJSON形式で実施するか(boolean)
    - `LOG_USE_STDOUT`: ログ出力を標準出力するか(boolean)
    - `OTLP_ADDR`(テスト中): otlpによるリクエストメトリクスの取得 (ZIPKIN_ADDR)
  - ジョブキュー設定
    - `STRAGE_TYPE`
      - `Standalone` RDBとメモリ(mpmcチャンネル)を利用する。単一インスタンスでの実行を想定した動作をする。(ビルド時にmysql指定をせずにSQLiteを利用すること)
      - `Scable`: RDBとRedisを利用する。複数インスタンスでの実行を想定した動作をする。(ビルド時に`--features mysql`を指定してrdbとしてmysqlを利用すること)
    - `JOB_QUEUE_EXPIRE_JOB_RESULT_SECONDS`: response_typeがLISTEN_AFTERの場合に結果を待つ最大時刻
    - `JOB_QUEUE_FETCH_INTERVAL`: rdbに格納されたjobの定期fetchの時間間隔
    - `STORAGE_REFLESH_FROM_RDB`: クラッシュ等で処理されなかったジョブが queue_type=WITH_BACKUP でrdbに残っているときにtrue指定することでredisに再度登録しなおして処理再開できる
  - GRPC設定
    - `GRPC_ADDR`: grpcサーバアドレス:ポート
    - `USE_GRPC_WEB`: grpcサーバでgRPC webを利用するか(boolean)

## プラグインについて

- [Runner trait](infra/src/infra/runner/plugins.rs) をdylibとして実装する
  - 環境変数 `PLUGINS_RUNNER_DIR` に指定したディレクトリ内に配置することでworker_Schemaとして登録される
  - 実装例：[HelloPlugin](plugins/hello_runner/src/lib.rs)

### 各種エラーコードについて

TBD

## その他
- cargoでのビルド時に`--feature mysql` を指定するとrdbとしてmysqlを利用する。指定しないとrdbとしてSQLite3を利用する。
- 定期実行ジョブのperiodic(繰り返しの時間(ミリ秒))の指定として.env のJOB_QUEUE_FETCH_INTERVAL(rdbへの定期ジョブ取得クエリ間隔)より短かい指定はできない
  - 時刻指定ジョブについてはrdbからプリフェッチをするためfetchと実行時間にずれがある場合にでも時間通りの実行が可能
- workerはSIGINT (Ctrl + c) シグナルにより実行中のjobの実行終了を待って終了する
- job idにはsnowflakeを利用。マシンidとして10bit各ホストのIPv4アドレスのホスト部を利用しているため、10bitを越えるホスト部を持つサブネットでの運用あるいは異なるサブネットで同一ホスト部を持つようなインスタンスを利用するような運用は避けてください。(重複したjob idを払いだす可能性があります)
- worker.type = DOCKER をk8s環境上のworkerで実行する場合にはDocker Outside Of Dockerの設定あるいはDocker in Dockerの設定が必要になります (未テストです)
- runnerでpanicを起こすとおそらくworkerプロセス自体が落ちる状態になっています。そのためworkerはsupervisordやkubernetes deploymentなどの耐障害性のある運用をすることが推奨されます。(C-unwind の適用検討は今後の課題です)


*Table of Contents: generated with [DocToc](https://github.com/thlorenz/doctoc)*
