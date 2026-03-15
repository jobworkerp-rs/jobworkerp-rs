# クライアントによる実行例

[jobworkerp-client](https://github.com/jobworkerp-rs/jobworkerp-client-rs)をつかって以下のようにworkerの作成・取得、jobのenqueue、処理結果の取得が可能です。

## セットアップ

```shell
# clone
$ git clone https://github.com/jobworkerp-rs/jobworkerp-client-rs
$ cd jobworkerp-client-rs

# build
$ cargo build --release

# run (show help)
$ ./target/release/jobworkerp-client

# list runner (need launching jobworkerp-rs in localhost:9000(default))
$ ./target/release/jobworkerp-client runner list
```

## 通常ジョブ実行（結果直接取得）

```shell
# create worker (specify runner id from runner list)
1. $ ./target/release/jobworkerp-client worker create --name "ExampleRequest" --description "" --runner-id 2 --settings '{"base_url":"https://www.example.com/search"}' --response-type DIRECT

# enqueue job
# specify worker_id value or worker name created by `worker create` (command 1. response)
2-1. $ ./target/release/jobworkerp-client job enqueue --worker 1 --args '{"headers":[],"method":"GET","path":"/search","queries":[{"key":"q","value":"test"}]}'
2-2. $ ./target/release/jobworkerp-client job enqueue --worker "ExampleRequest" --args '{"headers":[],"method":"GET","path":"/search","queries":[{"key":"q","value":"test"}]}'
```

## リアルタイム結果通知ジョブ

```shell
# create shell command `sleep` worker (must specify store_success and store_failure to be true)
1. $ ./target/release/jobworkerp-client worker create --name "SleepWorker" --description "" --runner-id 1 --settings '' --response-type NO_RESULT --broadcast-results --store-success --store-failure

# enqueue job
# sleep 60 seconds
2. $ ./target/release/jobworkerp-client job enqueue --worker 'SleepWorker' --args '{"command":"sleep","args":["60"]}'

# listen job (long polling with grpc)
# specify job_id created by `job enqueue` (command 2. response)
3. $ ./target/release/jobworkerp-client job-result listen --job-id <got job id above> --timeout 70000 --worker 'SleepWorker'
# (The response is returned as soon as the result is available, to all clients to listen. You can request repeatedly)
```

## 定期実行ジョブ

```shell
# create periodic worker (repeat per 3 seconds)
1. $ ./target/release/jobworkerp-client worker create --name "PeriodicEchoWorker" --description "" --runner-id 1 --settings '' --periodic 3000 --response-type NO_RESULT --store-success --store-failure --broadcast-results

# enqueue job (echo Hello World !)
# start job at [epoch second] % 3 == 1, per 3 seconds by run_after_time (epoch milliseconds) (see info log of jobworkerp all-in-one execution)
# (If run_after_time is not specified, the command is executed repeatedly based on enqueue_time)
2. $ ./target/release/jobworkerp-client job enqueue --worker 'PeriodicEchoWorker' --args '{"command":"echo","args":["Hello", "World", "!"]}' --run-after-time 1000

# listen by worker (stream)
$ ./target/release/jobworkerp-client job-result listen-by-worker --worker 'PeriodicEchoWorker'

# stop periodic job
# specify job_id created by `job enqueue` (command 2. response)
3. $ ./target/release/jobworkerp-client job delete --id <got job id above>
```
