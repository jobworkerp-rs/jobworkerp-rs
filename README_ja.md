# jobworkerp-rs

## 概要

jobworkerp-rs(ジョブワーカープラスと読みます)はRust製ジョブワーカーシステムです。
実行する処理を[worker](proto/protobuf/jobworkerp/service/worker.proto)として定義し、workerに対して実行命令となる[job](proto/protobuf/jobworkerp/service/job.proto)を登録(enqueue)することでジョブを実行できます。
プラグイン形式でworkerの実行機能(runner)を追加することができます。

### 主な機能

- 3種類のジョブキュー: Redis, RDB(mysql or sqlite), Hybrid (Redis + mysql)
  - rdbを利用することで必要に応じてジョブのバックアップをとりつつジョブ実行することができます
- 3種類の結果取得方法: 直接(DIRECT)、後から取得(LISTEN_AFTER)、結果取得しない(NONE)
- ジョブ実行チャネルの設定とチャネル毎の並列実行数の設定
  - 例えばgpuチャネルでは並列度1で実行、通常チャネルでは並列度4で実行などの設定ができます
- 指定時刻実行、一定間隔での定期実行
- リトライ: リトライ回数や間隔の設定 (Exponential backoffなど)
- プラグインによる実行ジョブ内容(Runner)の拡張

<!-- START doctoc generated TOC please keep comment here to allow auto update -->
<!-- DON'T EDIT THIS SECTION, INSTEAD RE-RUN doctoc TO UPDATE -->

**Table of Contents**

- [概要](#%E6%A6%82%E8%A6%81)
- [Command Examples](#command-examples)
  - [起動(ビルド)例](#%E8%B5%B7%E5%8B%95%E3%83%93%E3%83%AB%E3%83%89%E4%BE%8B)
    - [RDBの定義:](#rdb%E3%81%AE%E5%AE%9A%E7%BE%A9)
  - [grpcurl による実行例](#grpcurl-%E3%81%AB%E3%82%88%E3%82%8B%E5%AE%9F%E8%A1%8C%E4%BE%8B)
    - [注意](#%E6%B3%A8%E6%84%8F)
- [jobworkerp-workerの機能詳細](#jobworkerp-worker%E3%81%AE%E6%A9%9F%E8%83%BD%E8%A9%B3%E7%B4%B0)
  - [worker.runner_typeの種別](#workerrunner_type%E3%81%AE%E7%A8%AE%E5%88%A5)
  - [ジョブキュー種別 (config:storage_type、worker.queue_type):](#%E3%82%B8%E3%83%A7%E3%83%96%E3%82%AD%E3%83%A5%E3%83%BC%E7%A8%AE%E5%88%A5-configstorage_typeworkerqueue_type)
  - [結果の格納 (worker.store_success、worker.store_failure):](#%E7%B5%90%E6%9E%9C%E3%81%AE%E6%A0%BC%E7%B4%8D-workerstore_successworkerstore_failure)
  - [結果の取得方法 (worker.response_type):](#%E7%B5%90%E6%9E%9C%E3%81%AE%E5%8F%96%E5%BE%97%E6%96%B9%E6%B3%95-workerresponse_type)
- [その他](#%E3%81%9D%E3%81%AE%E4%BB%96)
  - [worker定義](#worker%E5%AE%9A%E7%BE%A9)
  - [その他の機能](#%E3%81%9D%E3%81%AE%E4%BB%96%E3%81%AE%E6%A9%9F%E8%83%BD)
- [プラグインについて](#%E3%83%97%E3%83%A9%E3%82%B0%E3%82%A4%E3%83%B3%E3%81%AB%E3%81%A4%E3%81%84%E3%81%A6)
- [仕様の詳細、制限事項](#%E4%BB%95%E6%A7%98%E3%81%AE%E8%A9%B3%E7%B4%B0%E5%88%B6%E9%99%90%E4%BA%8B%E9%A0%85)
  - [env.STORAGE_TYPEとworker.queue_typeの組み合わせと利用するqueueについて](#envstorage_type%E3%81%A8workerqueue_type%E3%81%AE%E7%B5%84%E3%81%BF%E5%90%88%E3%82%8F%E3%81%9B%E3%81%A8%E5%88%A9%E7%94%A8%E3%81%99%E3%82%8Bqueue%E3%81%AB%E3%81%A4%E3%81%84%E3%81%A6)
  - [利用するenv.STORAGE_TYPEとworker.response_typeとの組み合わせおよびJobResultService::Listenの挙動について](#%E5%88%A9%E7%94%A8%E3%81%99%E3%82%8Benvstorage_type%E3%81%A8workerresponse_type%E3%81%A8%E3%81%AE%E7%B5%84%E3%81%BF%E5%90%88%E3%82%8F%E3%81%9B%E3%81%8A%E3%82%88%E3%81%B3jobresultservicelisten%E3%81%AE%E6%8C%99%E5%8B%95%E3%81%AB%E3%81%A4%E3%81%84%E3%81%A6)
    - [表の表記と内容の詳細について](#%E8%A1%A8%E3%81%AE%E8%A1%A8%E8%A8%98%E3%81%A8%E5%86%85%E5%AE%B9%E3%81%AE%E8%A9%B3%E7%B4%B0%E3%81%AB%E3%81%A4%E3%81%84%E3%81%A6)
  - [各種エラーコードについて](#%E5%90%84%E7%A8%AE%E3%82%A8%E3%83%A9%E3%83%BC%E3%82%B3%E3%83%BC%E3%83%89%E3%81%AB%E3%81%A4%E3%81%84%E3%81%A6)
  - [その他の状況、今後の予定](#%E3%81%9D%E3%81%AE%E4%BB%96%E3%81%AE%E7%8A%B6%E6%B3%81%E4%BB%8A%E5%BE%8C%E3%81%AE%E4%BA%88%E5%AE%9A)

<!-- END doctoc generated TOC please keep comment here to allow auto update -->

## Command Examples

--------

### 起動(ビルド)例

```shell
# Run all-in-one binary from cargo (RDB storage: sqlite3)
$ cargo run --bin all-in-one

# build release binaries
$ cargo build --release

# prepare .env file for customizing settings
$ cp dot.env .env
# (modify to be appliable to your environment)

# Run the all-in-one server by release binary
$ ./target/release/all-in-one

# Run gRPC front server and worker by release binary
$ ./target/release/worker &
$ ./target/release/grpc-front &

```

### grpcurl による実行例

[protoファイル](proto/protobuf/jobworkerp/service/)

one shot job (no result)

```shell

# create worker

1. $ grpcurl -d '{"name":"EchoWorker","type":"COMMAND","operation":"echo","next_workers":[],"retry_policy":{"type":"EXPONENTIAL","interval":"1000","max_interval":"60000","max_retry":"3","basis":"2"},"store_failure":true}' \
    -plaintext \
    localhost:9000 jobworkerp.service.WorkerService/Create

# enqueue job (echo 'こんにちわ!')
# specify worker_id created by WorkerService/Create (command 1. response)
2. $ grpcurl -d '{"arg":"44GT44KT44Gr44Gh44KP77yBCg==","worker_id":{"value":"1"},"timeout":"360000","run_after_time":"3000"}' \
    -plaintext \
    localhost:9000 jobworkerp.service.JobService/Enqueue

```

one shot job (listen result)

```shell

# create sleep worker (need store_success and store_failure to be true in rdb storage)
1. $ grpcurl -d '{"name":"ListenSleepResultWorker","type":"COMMAND","operation":"sleep","next_workers":[],"retry_policy":{"type":"EXPONENTIAL","interval":"1000","max_interval":"60000","max_retry":"3","basis":"2"},"response_type":"LISTEN_AFTER","store_success":true,"store_failure":true}' \
    -plaintext \
    localhost:9000 jobworkerp.service.WorkerService/Create

# enqueue job
# specify worker_id created by WorkerService/Create (command 1. response)
# (timeout value(milliseconds) must be greater than sleep time)
2. $ grpcurl -d '{"arg":"MjAK","worker_id":{"value":"2"},"timeout":"22000"}' \
    -plaintext \
    localhost:9000 jobworkerp.service.JobService/Enqueue

# listen job
# specify job_id created by JobService/Enqueue (command 2. response)
$ grpcurl -d '{"job_id":{"value":"<got job id above>"},"worker_id":{"value":"2"},"timeout":"22000"}' \
    -plaintext \
    localhost:9000 jobworkerp.service.JobResultService/Listen

# (The response is returned as soon as the result is available)
    
```

periodic job

```shell

# create periodic worker (repeat per 3 seconds)
$ grpcurl -d '{"name":"EchoPeriodicWorker","type":"COMMAND","operation":"echo","retry_policy":{"type":"EXPONENTIAL","interval":"1000","max_interval":"60000","max_retry":"3","basis":"2"},"periodic_interval":3000,"store_failure":true}' \
    -plaintext \
    localhost:9000 jobworkerp.service.WorkerService/Create

# enqueue job (echo 'こんにちわ!')
# specify worker_id created by WorkerService/Create (↑)
# start job at [epoch second] % 3 == 1, per 3 seconds by run_after_time (epoch milliseconds) (see info log of jobworkerp-worker)
# (If run_after_time is not specified, the command is executed repeatedly based on enqueue_time)
$ grpcurl -d '{"arg":"44GT44KT44Gr44Gh44KP77yBCg==","worker_id":{"value":"10"},"timeout":"60000","run_after_time":"1000"}' \
    -plaintext \
    localhost:9000 jobworkerp.service.JobService/Enqueue
```

#### 注意

- RDBとしてSQLiteを利用する場合は並列実行性能が高くないため、並列度の高いヘビーな用途の利用ではredisとの併用やMySQLの利用を推奨します。
- periodic_intervalの指定として.env のJOB_QUEUE_FETCH_INTERVALより短かい指定はできません。

## jobworkerp-workerの機能詳細

workerは実行する仕事を定義します。runnerはworkerの定義に則してジョブを実行し、結果を得ます。
実行する機能としては worker.runner_type で現在5種類から選択できます。

### worker.runner_typeの種別

- COMMAND: command実行 ([ComamndRunner](worker-app/src/worker/runner/impls/command.rs)): worker.operationに対象のコマンドを指定、job.argに引数を指定する
- REQUEST: reqwestによるhttpリクエスト ([RequestRunner](worker-app/src/worker/runner/impls/request.rs)): worker.operationにbase url、job.argにjson形式でheaders、queries、method、body、pathを指定する (例: `{"headers":{"Content-Type":["plain/text"]},"queries":[["q","rust"],["ie","UTF-8"]],"path":"search","method":"GET"}`) 。レスポンス本文を結果として受け取る
- GRPC_UNARY: gRPC unaryリクエスト ([GrpcUnaryRunner](worker-app/src/worker/runner/impls/grpc_unary.rs)): worker.operationにjson形式でurlとpathを指定する (例: `{"url":"http://localhost:9000","path":"jobworkerp.service.WorkerService/FindList"}`)。job.argはrpc引数をprotobufエンコード(bytes)で指定する。レスポンスはprotobuf バイナリを受けとる。
- DOCKER: docker run実行 ([DockerRunner](worker-app/src/worker/runner/impls/docker.rs)): worker.operationにjson形式でFromImage (pullするイメージ)、Repo (レポジトリ)、Tag、Platform(`os[/arch[/variant]]`)を指定します(全てoptional。実行の事前準備のために指定します)。例:`{"FromImage":"busybox:latest"}`。job.argsにImage(実行するイメージ名)とCmd(実行コマンドラインの配列)をJson形式で指定します (例: `{"Image":"busybox:latest","Cmd": ["ls", "-alh", "/"]}`)
  - .envに DOCKER_GIDを指定する必要があります。
    - /var/run/docker.sock に接続する権限をもったGIDを指定します。実行プロセスとしてこのGIDを利用可能な権限が必要です。
  - k8s pod上での起動は現在未テストです。(上記の制限からおそらくDockerOutsideOfDockerあるいはDockerInDockerが可能なdocker imageの設定が必要になります)。
- PLUGIN:  pluginの実行 ([PluginRunner](worker-app/src/plugins/runner.rs)): worker.operationに実装したpluginのrunner.nameを指定、job.argにplugin runnerに渡すargを指定してください。
  - [サンプル](plugins/hello_runner/Cargo.toml)

### ジョブキュー種別 (config:storage_type、worker.queue_type)

- RDB (MySQL or SQLite: ヘビーな並列実行が必要な場合はMySQLをお勧めします)
- Redis (ナイーブな実装があるのであまりお勧めしません: run_after_time、periodic_interval指定時の処理やworker定義、job_resultの管理部分)
- Redis + RDB (Hybridモード: 遅延実行や定期実行、RedisのバックアップをRDB、即時実行をRedisで実行する)

### 結果の格納 (worker.store_success、worker.store_failure)

- worker.store_success、worker.store_failureの指定により実行成功、失敗時にstorage(Redisの場合はRedisのみ、その他はRDBテーブルjob_result) に保存されます
- [JobResultService](proto/protobuf/jobworkerp/service/job_result.proto)で取得できます

### 結果の取得方法 (worker.response_type)

- 結果取得なし (NO_RESULT): (デフォルト値) Job IDがレスポンスで返される。結果を格納している場合はjobの終了後に [JobResultService/FindListByJobId](proto/protobuf/jobworkerp/service/job_result.proto) をつかって取得できます。
- 後で取得(LISTEN_AFTER): enqueue時にはNO_RESULTと同様のレスポンス後、[job_result](proto/protobuf/jobworkerp/service/job_result.proto)サービスのListenをつかってロングポーリングする形で結果がでた際に取得できます。
  - Redis pubsubで結果を伝達するため複数のクライアントがListenして結果を得ることができます
- 直接取得(DIRECT): enqueueで結果がでるまで待ち、レスポンスとして直接結果が返されます。(結果を格納していない場合はリクエストしたクライアントのみ結果を取得できます)

## その他

### worker定義

- run_after_time (ミリ秒のエポックタイム) を指定することによりジョブの実行時刻を指定できます。
- timeout 時間を指定することができます。
- worker.periodic_intervalを指定することでその時間間隔で繰り返しのジョブを実行することができます
- 実行チャネル名および並列実行数を指定して、同名のチャネルを利用している特定のworkerで指定の並列度で処理を実行させることが可能です
- worker.retry_policyの指定でjob実行失敗時のリトライの方式(CONSTANT、LINEAR、EXPONENTIAL)や最大回数、最大時間間隔などを指定することができます
- 実行成功あるいは失敗の場合に結果をRDBに保存するように指定することができます
- worker.next_workersを定義することでジョブの実行結果を引数として更に別のworkerに処理引数として渡してジョブを連鎖させることができます (worker.idを数値でカンマ区切りで指定)
  - 例: 結果としてSlackResultNotificationRunner(worker_id=-1)を指定して結果をslack通知: worker.next_workers="-1"
- ビルトインworker (特定機能を実行するworkerを特定のworkerId指定で直接利用可能)が下記1つ利用可能です
  - slackによる結果通知 (SlackResultNotificationRunner: worker_id=-1): 各種時刻情報とともにjob.argに指定したものが本文として通知されます。
    - 環境変数にSLACK_で始まる設定が必要になります ([例](dot.env))
- (テスト中) runnerプロセスを並列度の分だけstaticに確保することが可能 (worker.use_static)
  - worker.use_static=trueに指定することでrunerをpoolingして初期化を都度行わないで使いまわします。

### RDBの定義

- [MySQL schema](infra/sql/mysql/002_worker.sql)
- [SQLite schema](infra/sql/sqlite/001_schema.sql)

### その他の機能

- workerはSIGINT (Ctrl + c) シグナルにより実行中のjobの実行終了を待って終了します。
- jaeger、zipkinによるリクエストメトリクスの取得 (otlpは現在テスト中)
- worker情報についてメモリキャッシュを持っています。rpcによる変更に応じてキャッシュを揮発しています (Redisが利用できる場合にはpubsubをつかって各instanceのメモリキャッシュを揮発させています)
- ログの出力に関して.envファイルで設定が可能です。ログのファイル出力時はファイル名に各ホストのIPv4アドレスに応じたサフィックスが付きます。(共有ストレージへの出力時に同一ファイルへ書き込みをしないようにしてファイルが壊れないようにしています)
- jobworkerp-frontは設定によってgRPC webを利用可能です。

## プラグインについて

Runner traitを実装したdylibを.envのPLUGINS_RUNNER_DIRに指定したディレクトリ内に配置してください。
workerの起動時にロードされます。

実装例: [HelloPluginRunner](plugins/hello_runner/src/lib.rs)

## 仕様の詳細、制限事項

### env.STORAGE_TYPEとworker.queue_typeの組み合わせと利用するqueueについて

worer.periodic_interval あるいは job.run_after_time の値を指定した場合(*) はRDBが利用可能な場合はRDBをqueueとして利用します。

(*) より詳細には worer.periodic_interval あるいは (job.run_after_time - 現在時刻のミリ秒) が .envのJOB_QUEUE_FETCH_INTERVALより大きいとき

|      | STORAGE_TYPE.rdb | STORAGE_TYPE.redis | STORAGE_TYPE.hybrid |
|----|----|----|----|
| QueueType::RDB    | RDBを利用 | エラー        | RDBを利用           |
| QueueType::REDIS  | エラー    | Redisを利用   | Redisを利用         |
| QueueType::HYBRID | RDBを利用 | Redisを利用   | Redisを利用 + RDBへバックアップ |

- Redisを利用: RedisのRPUSH、BLPOPを使ってjobのキュー操作をします (ジョブのタイムアウト時のリカバリなし)
- RDBを利用: RDBを一定間隔(env.JOB_QUEUE_FETCH_INTERVAL)でfetch、grabbled_until_timeの更新によるジョブの確保をすることでjobのキュー操作をします。(ジョブのタイムアウト時のリカバリあり)
- Redisを利用 + RDBへバックアップ: RedisのRPUSH、BLPOPを使ってjobのキュー操作をします。ジョブがタイムアウトした場合や強制再起動した場合などRedisからjobをpopしたままタイムアウト時刻をすぎても実行完了しなかった場合にはRDBから自動リカバリして再実行されます。env.JOB_QUEUE_WITHOUT_RECOVERY_HYBRID=trueを指定すると自動リカバリ機能をOFFにすることもできます。

なおJobRestoreService.Restore は実行時、env.STORAGE_REFLESH_FROM_RDB=true はworker起動時1回のみ同様に保存されているタイムアウトジョブ全てをRDBからRedisへリストアします (大量にqueueにジョブがある場合には重い処理になります)。

### 利用するenv.STORAGE_TYPEとworker.response_typeとの組み合わせおよびJobResultService::Listenの挙動について

基本的には(worker.store_success、worker.store_failure設定により)storeしたものをJobResultService::Find\*メソッドで取得できますがworker.response_typeの設定によってはJobResultService::Listenにより以下のように結果を取得できます。
(基本的にRedisを利用可能かどうかによって挙動が異なります。RDBのみの利用の場合はRDBに保存しない設定の場合は結果を返せないためエラーになります。)

|      | STORAGE_TYPE.rdb | STORAGE_TYPE.redis | STORAGE_TYPE.hybrid |
|----|----|----|----|
| response_type::NO_RESULT | store_\*=trueではないworkerを指定するとエラー | エラー | エラー |
| response_type::LISTEN_AFTER | store_\*=trueではないworkerを指定するとエラー | 所定の時間だけListenで取得可能 | 所定の時間だけListenで取得可能 |
| response_type::DIRECT | store_\*=trueではないworkerを指定するとエラー | エラー | エラー |

#### 表の表記と内容の詳細について

- エラー : Jobの終了を検知しないためListenはできない組み合わせになっています (InvalidParameterエラーとなります)。
- store_\*=trueではないworkerを指定するとエラー : worker.store_success、worker.store_failure の両方がtrueのworkerを指定しない場合はエラーになります。JobResultService::Listen を使うと結果がでたタイミングでレスポンスを返します (RDBの場合は定期fetchのため結果がでて最大JOB_QUEUE_FETCH_INTERVALだけ時間かかります)
- 所定の時間だけListenで取得可能: 結果がでてからJOB_QUEUE_EXPIRE_JOB_RESULT_SECONDSの間はJobResultService::Listenによる結果取得が可能です (結果がでる前にリクエストしていた場合は結果がでたタイミングでレスポンスが返ってきます)。それ以降は"find*と同様" と同じ動作になります。(Listen用にRedisにexpire付きでjobResultが保存されます)
- reponse_type::LISTEN_AFTERはRedisを利用できる場合はpubsubを用いてlistenしているクライアントが結果をsubscribeすることで実現しています。response_type::LISTEN_AFTER 以外ではpubsubを実行しないためListenできません。RDBでは単にstoreされる結果をループしてfetchして待機しているだけのため結果的にどのresponse_typeでもListenが可能です。(敢えてエラーにすることもしていません)

### 各種エラーコードについて

TBD

## その他の状況

- env.STORAGE_TYPE=redis は情報取得などで一部よくない実装があります。ジョブキューが大量に詰まっているなどの特殊なケースでは問題になる可能性があります。
- job idの払いだしにはsnowflakeを利用しています。マシンidとして10bit各ホストのIPv4アドレスのホスト部を利用しているため、10bitを越えるホスト部を持つサブネットでの運用あるいは異なるサブネットで同一ホスト部を持つようなインスタンスを利用するような運用をしてしまうと重複したjob idを払いだす可能性がありますので避けてください (あるいはjob idの重複によるAlreadyExistsエラーがでなくなるまでJobService.Enqueueをリトライしてください)。
- worker.type = DOCKER をk8s環境上のworkerで実行する場合にはDocker Outside Of Dockerの設定あるいはDocker in Dockerの設定が必要になります (未テストです)

## 今後の予定

(自分以外の利用者がいるようならやるかもしれません)

- 追加で必要そうなrpcの追加 (例：JobService/FindListByWorkerId)
- Redis clusterへの対応: redis-rs が [pubsub対応したら](https://github.com/redis-rs/redis-rs/issues/492) 対応する予定です
  - redis pubsubの利用箇所: worker定義を変更したときにはredisのpubsubにより各サーバへ更新が通知されキャッシュが揮発します。またresponse_type=LISTEN_AFTERの時の結果はpubsubによって各frontへ結果が通知されます。
- OpenTelemetry Collectorへの対応: 現在実装のみで未テストです。
- runnerでpanicを起こすとおそらくworkerプロセス自体が落ちる状態になっています。そのためworkerはsupervisordやkubernetes deploymentなどの耐障害性のある運用をすることが推奨されます。(C-unwind の適用検討は今後の課題です)
- ドキュメントの充実

*Table of Contents: generated with [DocToc](https://github.com/thlorenz/doctoc)*
