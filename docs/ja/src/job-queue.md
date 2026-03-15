# ジョブキューと結果取得

## ジョブキュー種別

環境変数`STORAGE_TYPE`で全体の動作モードを設定します：

- **Standalone**: 即時ジョブは memory(mpsc, mpmc channel) 、時刻指定ジョブなどはrdb(sqlite, mysql)に格納するためシングルインスタンスでの実行のみサポート
- **Scalable**: 即時ジョブは redis 、時刻指定ジョブなどはrdb(mysql)に格納するためgrpc-front、workerをそれぞれ複数台で構成することができる
  - cargoでのビルド時に `--features mysql` を付けてビルドする必要があります

worker.queue_typeでは個別のジョブキュー種別を指定します：

- **NORMAL**: 即時実行ジョブ(時刻指定のない通常のジョブ)はchannel (redis) に、定期実行や時刻指定ジョブはdbに格納
- **WITH_BACKUP**: 即時実行ジョブをchannelとrdbの両方に格納する(障害時にrdb内のジョブをリストアできる)
- **DB_ONLY**: 即時実行ジョブもrdbのみに格納する (実行が遅くなることがある)

## 結果の格納と取得

### 結果の格納

- worker.store_success、worker.store_failureの指定により実行成功、失敗時にrdb(job_resultテーブル) に保存
- [JobResultService](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/proto/protobuf/jobworkerp/service/job_result.proto)で実行後に参照できます

### 結果の取得方法

worker.response_typeにより以下の取得方法があります：

- **結果取得なし (NO_RESULT)**: (デフォルト値) Job IDがレスポンスで返される。結果を格納している場合はjobの終了後に [JobResultService/FindListByJobId](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/proto/protobuf/jobworkerp/service/job_result.proto) をつかって取得できる。
- **直接取得(DIRECT)**: enqueueリクエストで実行完了まで待ち、そのレスポンスとして直接結果が得られる。(結果を格納していない場合はリクエストしたクライアントのみ結果を取得可能)

また、worker.broadcast_resultsを有効にすると:

- 実行結果の即時通知: enqueue後、[job_result](https://github.com/jobworkerp-rs/jobworkerp-rs/blob/main/proto/protobuf/jobworkerp/service/job_result.proto)サービスのListenを使って実行終了後すぐに結果を取得できる。(ロングポーリング方式)
  - 複数のクライアントがListenして全クライアントが同じ結果を得ることができます (Redis pubsubでの伝達)
  - 使用例は[クライアント実行例](client-usage.md)の「リアルタイム結果通知ジョブ」を参照してください
- **ストリーミング結果の取得** (JobResultService.ListenStream): [ストリーミング](streaming.md)が有効なジョブ（`streaming_type` = ResponseまたはInternal）の場合、gRPCサーバーストリーミングで`ResultOutputItem`ストリームとして逐次的な実行結果を取得できます。`JobResult`メタデータ（ステータス、タイムスタンプ等）はgRPCレスポンスヘッダー（`x-job-result-bin`）に格納され、ストリーム本体では`Data`チャンク、`End`トレーラー、`FinalCollected`（Internalモードのみ）が配信されます。Listenと同じ`ListenRequest`を使用します。
- 特定workerの実行結果をストリームとして取得しつづけられます (JobResultService.ListenByWorker)
