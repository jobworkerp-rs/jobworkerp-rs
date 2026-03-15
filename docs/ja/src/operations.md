# 運用上の注意点

- シグナルによるシャットダウン
  - SIGINT (Ctrl+C) および SIGTERM を受信すると、実行中のジョブの完了を待ってからシャットダウンします
  - all-in-oneモード（`all-in-one`バイナリ）では、シグナル受信後5秒のタイムアウトで強制終了します
  - worker/grpc-front単体起動の場合は、タイムアウトなしでジョブの完了を待ちます
  - コマンド実行Runner (COMMAND/PYTHON_COMMAND) は子プロセスにSIGTERMを送信後、5秒待ってからSIGKILLで強制終了します
- 定期実行ジョブの`periodic_interval`（繰り返し間隔、ミリ秒）は`JOB_QUEUE_FETCH_INTERVAL`（RDBへの定期ジョブ取得クエリ間隔、デフォルト1000ms）より大きい値を指定する必要があります
  - 時刻指定ジョブについてはRDBからプリフェッチを行うため、fetch間隔と実行時間にずれがある場合でも指定時刻通りに実行されます
- IDの生成
  - ID（job ID等）にはSnowflakeを利用し、マシンIDとしてIPv4アドレスの下位10ビットを使用します
  - 10ビットを超えるホスト部を持つサブネットでの運用や、異なるサブネットで同一ホスト部を持つインスタンスの利用は重複IDの原因となるため避けてください
  - IPv6環境ではランダムな値にフォールバックします
- jobworkerp-rsをKubernetes環境にデプロイし、DOCKERランナーを利用する場合は、Pod内からDocker APIにアクセスするためにDocker Outside of Docker (DooD) またはDocker in Docker (DinD) の設定が必要です（未検証）
- Runner Plugin内の処理でpanicが発生するとWorkerプロセス全体がクラッシュします。Workerはsupervisord、Kubernetes Deployment等の耐障害性のある仕組みで運用することを推奨します。詳細は[プラグイン開発](plugin-development.md)を参照してください
