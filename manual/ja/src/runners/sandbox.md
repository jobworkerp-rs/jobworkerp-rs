# SANDBOX Runner

`SANDBOX` は [microsandbox](https://docs.microsandbox.dev/) の microVM 内でコマンドを実行します。`COMMAND` は Worker ホスト上、`DOCKER` はコンテナ内、SANDBOX は VM 内で実行する点が異なります。

**VM の共有範囲は Worker 作成時の `use_static` で決まります。** ここで「Worker」は登録したジョブ実行設定、「Worker process」はホスト上で稼働するジョブ処理プログラムを指します。`false` ならジョブごとに新しい VM を作成・削除します。`true` なら同じ Worker process 内の同じ Worker が、後続のジョブでも VM を再利用します。ただし「1 Worker = 必ず 1 VM」ではありません。channel concurrency が `1` ならその Worker process 内で最大 1 台、`2` なら最大 2 台です。再利用される VM ではコマンドだけを新たに起動するため、ファイルの変更が次のジョブに残ります。どの VM を使うかは選べません。

## 前提

既存の DB では新しい migration を適用してから再起動してください。`RunnerService/FindList` は Runner の定義を DB から取得します。microsandbox の導入だけでは `SANDBOX` は一覧に表示されません。

- 実行ホストは Linux、KVM、および microsandbox runtime を必要とします。
- microsandbox Rust SDK 0.7.2 を使用します。runtime の UDP 同時接続数制限が非対応の場合、ネットワークを有効にした VM の起動は失敗します。
- v1 は microsandbox local backend を対象とします。
- ネットワークは既定で無効です。必要な通信だけを Worker 設定で許可してください。
- `use_static=true` では同じ VM を使用したジョブ間でファイルシステムを共有します。異なるテナント、認可境界、秘密情報を扱うジョブを同じ Worker に投入してはいけません。

### ローカル E2E 動作確認

1. `msb doctor` で KVM へのアクセスを確認し、`msb run alpine:3.21 --name <重複しない確認用の名前> -- sh -c 'printf ok'` が動作することを確認します。確認用 VM が残った場合は `msb list` で照合して削除してください。
2. テスト専用の Redis を用意します。テスト初期化時に Redis の **全 DB に対して `FLUSHALL`** が実行されるため、業務データや別プロセスと共有してはいけません。例: `redis-server --port 16389 --bind 127.0.0.1 --save '' --appendonly no --daemonize yes`。
3. `TEST_REDIS_URL=redis://127.0.0.1:16389 UV_VENV_CLEAR=1 cargo test -p tests-with-worker --test sandbox_worker_e2e_test -- --ignored --test-threads=1 --nocapture` を実行します。`SANDBOX` が一覧から見つかり、実 worker で VM 内のコマンドが成功して終了・出力が検証されます。CI では実行されません。
4. 専用 Redis を `redis-cli -p 16389 shutdown nosave` で停止します。テスト用ポートを変更した場合は同じポートを指定してください。

`use_static` は `SandboxRunnerSettings` ではなく、Worker 作成時の `worker.data.use_static` に設定します。

ネットワーク許可は Worker 側で設定します。ジョブごとに VM を作る場合は、そのジョブだけネットワークを無効化できます。static Worker は VM を共有するため、ジョブ単位のネットワーク切替はできません。ネットワーク無効の static Worker が必要な場合は、Worker 側で無効にしてください。

### Docker 内で使う場合

all-in-one と worker の GPU／CPU Dockerfile は microsandbox 0.7.2 の `msb` と対応する `libkrunfw` を実行イメージに配置し、`jobworkerp` ユーザー専用の `MSB_HOME` を使います。ホスト側の microsandbox のインストールをコンテナに共有する必要はありません。ただし Dockerfile だけでは KVM を公開できません。SANDBOX を使う環境では、それぞれの Compose override で `/dev/kvm` を渡します。通常の Compose 設定は KVM を要求しません。

1. Docker を動かす Linux ホスト（外側が VM の場合は nested virtualization）に `/dev/kvm` があり、Docker から利用できることを確認します。Docker Desktop 等でデバイスを公開できない環境では local backend は動きません。
2. 対象の Dockerfile でイメージをビルドします。all-in-one は `docker compose -f docker-compose.yml -f docker-compose-sandbox.yml up -d`、scalable worker は `docker compose -f docker-compose-scalable.yml -f docker-compose-scalable-sandbox.yml up -d` で起動します。起動後、同じ `-f` の組合せで `exec` を行い、all-in-one は `-u jobworkerp jobworkerp-all msb doctor`、worker は `jobworkerp-worker msb doctor` を実行してください。`KVM device` と `KVM access` が成功していることを確認します。
3. 同じユーザーで `msb run alpine:3.21 --name <重複しない確認用の名前> -- sh -c 'printf "sandbox-ok\n"'` を実行し、microVM 内の出力と終了コードを確認します。確認用 VM の record が残った場合は `msb list` で名前を照合して削除します。実行例は [公式 Docker 手順](https://docs.microsandbox.dev/examples/docker/docker) を参照してください。

ホストの `/dev/kvm` が特定のグループにのみ読み書きを許す場合、`stat -c '%g' /dev/kvm` で **そのデバイスの数値 group ID** を確認し、使う SANDBOX 用 Compose override の対象 service に `group_add: ["<数値 group ID>"]` を追加してください。既存の Docker socket 用グループとは別です。デバイス自体がない環境では `--privileged` を追加しても解決しません。VM image のキャッシュや sandbox database を再起動後も維持したい場合は、`MSB_HOME` に **Worker process ごとの専用 volume** をマウントし、`jobworkerp` が書き込めることを確認してください。異なる Worker process の間で書き込み可能な runtime 状態を共有しないでください。

## 実行方法

| `using` | 呼出 RPC | 入力 | TTY | 用途 |
| --- | --- | --- | --- | --- |
| `run` | `EnqueueForStream` | 通常のジョブ引数。必要なら固定 stdin | `tty=false` が既定 | 一回限りのコマンド実行 |
| `run_with_client` | `EnqueueWithClientStream` | feed による逐次 stdin | 常に有効 | coding agent、対話シェル、REPL |

両メソッドは stdout/stderr を `ResultOutputItem::Data` として逐次返し、終了時に `End` を返します。

- `EnqueueForStream` で `using` を省略すると `run` を使用します。ただし `JobService.Enqueue` は非ストリーミング実行専用のため、SANDBOX の `run` は呼び出せません。
- `run` では出力チャンク間に `JobRequest.timeout` で指定した時間だけ出力がないと実行をキャンセルし、`End.metadata["stream_error"]` に `TIMEOUT` を記録します。コマンド全体の上限を指定する `timeout_ms` とは別です。
- `run` は通常の enqueue を使用できないため、定期ジョブには使用できません。
- `run_with_client` も client streaming 専用のため、定期ジョブには使用できません。
- `run_with_client` は `EnqueueWithClientStream` 専用です。最初の `job_request.using` に `run_with_client` を指定し、以後に stdin のバイト列である `feed_data` を送信します。`is_final=true` では、通常の端末入力で EOF を伝える EOT を送って stdin を閉じます。ゲストが端末を raw mode に切り替えた場合、EOT は EOF として解釈されず、制御文字（`0x04`）としてアプリケーションへ届き、入力内容や挙動に影響することがあります。その場合はアプリケーション固有の終了操作か `timeout_ms` を使用してください。
- `run_with_client` は入力待ちで出力が途切れても `JobRequest.timeout` の出力間隔による制限では終了しません。`timeout_ms` または Worker 設定の `default_exec_timeout_ms` を指定すると、プロセス全体の実行時間を制限できます。両方未指定の場合、実行ごとの時間上限は設けません。`JobRequest.timeout` による stream 開始待ちの制限は引き続き適用されます。
- 異なる job ID の `run_with_client` セッションは並列に実行できます。channel concurrency を増やすと、static Worker では VM も増えます。同じ job ID を複数セッションで共有することはできません。
- `run_with_client` は PTY を使用するため、stdout と stderr は併合されて stdout として返ります。端末エコーや ANSI 制御文字を含む場合があります。

## 設定と引数

Worker 設定は `SandboxRunnerSettings`、ジョブ引数は `SandboxExecArgs` です。以下の名前は Runner 専用の protobuf message のフィールド名です。CLI 等が Runner schema から protobuf へ変換する際は snake_case の JSON を入力します。gRPC の `WorkerData.runner_settings` と `JobRequest.args` に直接 JSON を入れることはできず、protobuf にエンコードした `bytes` を送ります（[直接 gRPC API](../grpc-direct-api.md)）。

### Worker 設定

| フィールド | 型 | 必須 | 説明 |
| --- | --- | --- | --- |
| `vm` | `SandboxVmConfig` | はい | static VM または non-static VM の既定値と資源上限。 |
| `network` | `SandboxNetworkConfig` | いいえ | VM のネットワーク許可。省略時はネットワーク device を無効化。 |
| `allowed_images` | `repeated string` | はい | 空ではない OCI image 名の完全一致リスト。`vm.image` を含めます。 |
| `allowed_host_mounts` | `repeated SandboxAllowedHostMount` | いいえ | host bind mount の許可ルート。空なら mount 禁止。 |
| `default_exec_timeout_ms` | `optional uint64` | いいえ | ジョブが `timeout_ms` を指定しない場合だけ使用する実行時間の既定値。未指定なら上限なし。 |

### ジョブ引数

| フィールド | 型 | `run` | `run_with_client` | 説明 |
| --- | --- | --- | --- | --- |
| `command` | `string` | 必須 | 必須 | 空ではないリテラルの実行ファイル名またはゲスト内パス。シェル展開しません。 |
| `args` | `repeated string` | 任意 | 任意 | argv の後続要素。 |
| `working_dir` | `optional string` | 任意 | 任意 | ゲスト内の絶対パス。指定時は `vm.working_dir` に優先。 |
| `env` | `map<string, string>` | 任意 | 任意 | プロセス固有の環境変数。Worker の `vm.env` と同名のキーは指定不可。ジョブ `vm.env` との同名キーではこちらが優先。 |
| `user` | `optional string` | 任意 | 任意 | ゲスト内の実行ユーザー。 |
| `timeout_ms` | `optional uint64` | 任意 | 任意 | このプロセスの総実行時間の上限。正の値のみ。 |
| `treat_nonzero_as_error` | `bool` | 任意 | 任意 | 成功コード以外での終了をジョブ失敗として扱う。既定は false。 |
| `success_exit_codes` | `repeated int32` | 任意 | 任意 | エラー判定時の成功コード。空なら `[0]`。 |
| `stdin` | `optional bytes` | 任意 | 不可 | 固定 stdin。JSON 表現は Base64。空バイト列の明示指定も可能。 |
| `tty` | `optional bool` | 任意 | 不可 | true なら PTY を使用。省略時 false。 |
| `vm` | `SandboxVmConfig` | 条件付き | 条件付き | non-static の場合のみ、下記フィールドを個別に上書き。static は指定自体を拒否。 |
| `network` | `SandboxNetworkConfig` | 条件付き | 条件付き | non-static の場合のみ `{ "enabled": false }` を許可。static は指定自体を拒否。 |

`command` にはゲスト内の実行ファイル名を、`args` にはその引数を別々に指定します。`COMMAND` や `DOCKER` と同様、`|`、`>`、`&&` などは自動的にシェルで解釈されません。例えば `{"command":"sh","args":["-c","echo hello | wc -c"]}` のように、ゲストに存在するシェルを明示すればパイプを使えます。`COMMAND` は `args` が空なら `command` 内の空白や引用符を引数として分割しますが、SANDBOX にはその入力補助はありません。`command` と `args` を分けてください。

Worker 設定の CPU・メモリ・OCI root disk 容量は non-static job の上限兼既定値です。ジョブからは縮小のみ指定できます。一方 `timeout_ms` は実行コマンドごとに指定でき、Worker の `default_exec_timeout_ms` より長い値も指定できます。`JobRequest.timeout` は Runner の呼出しや stream の開始待ちなどに使われ、SANDBOX のプロセス全体の上限時間にはなりません。非ストリーミングの Runner では、`JobRequest.timeout` が `run()` の待機上限にもなります。SANDBOX の `run` と `run_with_client` はストリーミング専用です。

## VM とネットワーク

### `SandboxVmConfig`

Worker 側の `vm` は `image`、`cpus`、`memory_mib`、`root_disk_mib` が必須です。ジョブ側の `vm` は指定したフィールドだけを上書きし、省略フィールドは Worker の値を引き継ぎます。すべての数値は正の整数です。資源上限を省略して SDK 既定値に委ねることはできません。

| フィールド | 型 | 内容と non-static ジョブからの指定 |
| --- | --- | --- |
| `image` | `optional string` | OCI image 名。Worker の `allowed_images` に完全一致する場合のみ変更可能。ローカルパスは不可。 |
| `cpus` | `optional uint32` | vCPU 数。Worker の値以下、SDK の `u8` 範囲内で正数。 |
| `memory_mib` | `optional uint32` | VM メモリ MiB。Worker の値以下で正数。 |
| `root_disk_mib` | `optional uint32` | OCI writable root disk の容量 MiB。Worker の値以下で正数。 |
| `env` | `map<string, string>` | VM 内の全コマンドに渡す環境変数。ジョブが追加するキーは Worker の同名キーと重複不可。実行時の上書きは [ジョブ引数](#ジョブ引数)を参照。 |
| `working_dir` | `optional string` | ゲスト内の絶対パス。ジョブの指定で変更可能。 |
| `mounts` | `repeated SandboxBindMount` | Worker の mount に追加する bind mount。すべて `allowed_host_mounts` の権限内でなければならない。同一 guest path の重複は拒否。 |
| `max_duration_sec` | `optional uint64` | VM の稼働時間上限（秒）。ジョブでは Worker の指定値より短縮のみ可能。 |
| `idle_timeout_sec` | `optional uint64` | 実行中のコマンドがない状態が続く場合の自動停止時間（秒）。ジョブでは Worker の指定値より短縮のみ可能。 |

`max_duration_sec` と `idle_timeout_sec` はジョブの実行時間の合計ではなく VM の自動停止設定です。上限に達すると新しいコマンドを受け付けず、実行中のコマンドを終えてから停止します。コマンドの `timeout_ms` より長く設定する必要はありません。

再利用中の VM が自動停止してもファイル状態は残ります。次のジョブの前に **SANDBOX Runner が自動的に**同じ VM の状態を確認し、必要なら再開します。Worker の更新・削除などで再利用が終わると、VM とそのファイル状態の削除を試みます。VM の停止と削除は別の操作です。ジョブがない時間だけを理由に VM を削除する仕組みは、現在の Runner pool にはありません。

### bind mount の許可

| message | フィールドと型 | 説明 |
| --- | --- | --- |
| `SandboxAllowedHostMount` | `host_root: string`, `allow_write: bool`, `allow_exec: bool` | Worker 側の許可ルート。host root は存在する絶対パス。権限は省略時 false。 |
| `SandboxBindMount` | `host_path: string`, `guest_path: string`, `writable: bool`, `executable: bool` | host のファイルまたはディレクトリを guest の絶対パスに bind。`writable` と `executable` は省略時 false。 |

各 host path と Worker が許可した host root の双方を canonicalize し、許可ルートそのものかその配下であることを確認します。設定した mount は Worker 側もジョブ側も同じ検証を受けます。要求した書込み・実行権限が許可ルートを超える場合は拒否します。`nosuid,nodev` は常に適用し、`executable=false` は `noexec` を適用します。bind mount によるホストへの書込みは VM の削除でも取り消せません。

v1 は OCI image の書込み用 root disk 容量も指定できます。追加 volume、rootfs patch、custom init、microsandbox の `script` 機能、TLS interception、カスタム DNS、host port publish は指定できません。microsandbox の `script` は VM 作成時に名前付きスクリプトを配置して後から呼び出す機能であり、SANDBOX Runner の `command` で通常のスクリプトを実行することとは別です。シェルスクリプトはゲスト内に存在するシェルを `command` に指定して実行できます。

### `SandboxNetworkConfig`

| フィールド | 型 | 内容 |
| --- | --- | --- |
| `enabled` | `optional bool` | Worker では未指定・false なら network device を作らない。ジョブでは明示的な false のみ許可。 |
| `profiles` | `repeated string` | Worker だけが指定できる `public`、`private`、`host` の組合せ。指定 profile はカテゴリ全体への egress を許可。 |
| `rules` | `repeated SandboxNetworkRule` | Worker だけが指定できる順序付き egress allow/deny ルール。 |
| `max_tcp_connections` | `optional uint32` | ネットワーク有効時は正の有限値を必須とする TCP 同時接続数上限。 |
| `max_udp_connections` | `optional uint32` | ネットワーク有効時は正の有限値を必須とする UDP relay session 数上限。 |

| message | フィールドと型 | 内容 |
| --- | --- | --- |
| `SandboxNetworkRule` | `action: string`, `destination: SandboxNetworkDestination`, `protocols: repeated string`, `ports: repeated SandboxPortRange` | `action` は `allow` または `deny`。protocols は `tcp` / `udp` のみで、空なら両方。ports が空なら全 port。 |
| `SandboxNetworkDestination` | `oneof { group: string, ip: string, cidr: string, domain: string, domain_suffix: string, any: bool }` | group は `public` / `private` / `host`。`any` は true のみ。宛先は必ず一つだけ指定。 |
| `SandboxPortRange` | `start: uint32`, `end: uint32` | 両端を含む 1〜65535 の port 範囲。単一 port は start=end。 |

ネットワークを設定する際は、次の条件に従ってください。

- `network` を省略するか `enabled=false` にすると、ネットワークは無効です。その場合、profile、rules、接続数上限は併記できません。
- `enabled=true` にする場合は、TCP と UDP の接続数上限を両方指定してください。旧 microsandbox runtime は UDP 上限をサポートせず起動を拒否するため、対応 runtime が必要です。
- egress と ingress は、どの許可にも一致しなければ拒否します。Worker の `rules` は記載順に、次に `profiles` で生成したルールを評価し、最初に一致したものを使います。ingress の許可と host port 公開は指定できません。
- DNS が必要なら、gateway（宛先 `host`、TCP/UDP の port 53）への通信も許可してください。`profiles` を指定した場合は、microsandbox がそのための狭い許可ルールを自動生成します。

Worker 設定のルールでは全宛先を表す `any` も指定できます。**SANDBOX Runner は**クラウドの認証情報が得られる可能性のある metadata endpoint（`169.254.169.254`）への拒否ルールを最優先で追加するため、`allow any` を指定してもこの宛先には接続できません。`host` は VM の実行ホストへのアクセスであり、全宛先の許可を意味しません。microsandbox の DNS rebinding 防御は `allow any` だけでは解除されず、private IP に解決される名前を使う場合は `private` 等のアドレス対象ルールが別途必要です。

接続数上限の `0` は指定できません。microsandbox SDK では unlimited を意味しますが、SANDBOX Runner では正の有限値だけを許可します。ドメイン allow ルールは DNS 解決先の IP と SNI 等を確認しますが、TLS interception を公開しない v1 では HTTPS の暗号化された HTTP authority を検査できません。ドメイン名だけでアプリケーションの接続先を完全に制限できるとはみなさないでください。

bind mount の host path と guest path は絶対パスでなければなりません。host path は Worker 設定の許可ルート内である必要があります。すべての mount に `nosuid` と `nodev` を適用し、`noexec` を既定とします。`allowed_host_mounts` で `allow_exec=true` を許可したルートだけ `noexec` を解除できます。`noexec` はインタプリタ経由の script 実行を防ぐものではありません。

## VM の再利用とホスト容量

static Worker の設定更新、release、削除後は、新しい設定で VM が作り直されます。Worker process を再起動した場合、以前の static VM の状態は継承しません。channel concurrency を `2` 以上にすると複数 VM が存在するため、後続ジョブがどの VM を使用するかは保証されません。

microsandbox local backend では、**一つの sandbox が一つの独立した VM とホストプロセス**を持ちます。同じ VM へ複数回コマンドを実行しても、コマンドごとに VM ホストプロセスを作り直すわけではありません。SANDBOX の static Runner は一 instance ごとに一 VM を所有し、pool の貸出中は一度に一つのジョブがその VM を使います。別の Worker process が同じホスト上で動くときも、それぞれ別の VM・ホストプロセスを作成し、共通の VM プロセスや常駐 daemon は共有しません。SANDBOX は local attached VM のみを使用し、Worker process 終了後も VM を稼働させる detached mode は使いません。

ホストの容量を決める際は、一つの VM の `cpus`・`memory_mib`・`root_disk_mib` に、**同じホスト上で保持し得る VM 数**を掛けて上限を見積もってください。static Worker ごとに、pool は所属 channel の concurrency を上限として実行用の Runner を必要に応じて作ります。channel を指定しない Worker は既定 channel の concurrency を使います。例えば一つの Worker process が同じ concurrency `3` の channel に属する static Worker を二つ担当すると、需要次第で合計最大 `6` 台の VM を保持し得ます。別の Worker process が同じホストで動くならその分も加算します。non-static ジョブの一時的な VM や、設定更新時に旧・新 pool が並存する期間も考慮してください。vCPU 数とメモリ容量は VM ごとの上限であり、必ず同量のホスト CPU・物理メモリを予約する意味ではありません。

## 設定と実行の例

以下は Runner 固有 message の **protobuf 変換前の JSON** です。`runner_id` は環境ごとに Runner 一覧から取得し、`use_static` と response type は Worker 作成時の `WorkerData` に設定します。JSON の送信形式については[設定と引数](#設定と引数)を参照してください。

1. Linux/KVM ホストに microsandbox local runtime を用意し、Worker の作成権限を持つクライアントから `SANDBOX` の Runner ID と schema を取得します。
2. `use_static=false`、ストリーミング結果を扱える response type を選び、次の `SandboxRunnerSettings` を protobuf に変換して Worker の `runner_settings` に渡します。ネットワークと mount は無効です。

   ```json
   {
     "vm": {
       "image": "python:3.12",
       "cpus": 2,
       "memory_mib": 1024,
       "root_disk_mib": 8192,
       "working_dir": "/tmp",
       "idle_timeout_sec": 300
     },
     "allowed_images": ["python:3.12"],
     "default_exec_timeout_ms": 60000
   }
   ```

3. `EnqueueForStream` に `using="run"` と、次の `SandboxExecArgs` を protobuf に変換した `args` bytes を渡します。`JobRequest.timeout` は stream 開始待ち等のため別途設定できます。`timeout_ms` を省略したこの例では、コマンドの上限は Worker 側の 60000 ms です。

   ```json
   {
     "command": "python3",
     "args": ["-c", "import sys; print(sys.stdin.read().upper())"],
     "stdin": "aGVsbG8K",
     "vm": {"cpus": 1, "memory_mib": 512}
   }
   ```

   `stdin` は Base64 で表現した `hello\n` です。`run_with_client` を使う場合は同じ Worker へ `using="run_with_client"` の `job_request` を最初に送信し、`stdin` / `tty` は付けず、その後 `feed_data` に生バイト列と最後の `is_final=true` を送ります。RPC の構造は [ストリーミング](../streaming.md) を参照してください。

4. ネットワークを使う別の Worker は、Worker 設定の `network` に許可と接続数上限を明示します。次の例は公開アドレスの HTTPS と、名前解決のための gateway DNS のみを許可します。ジョブから宛先を追加することはできません。

   ```json
   {
     "vm": {"image": "python:3.12", "cpus": 2, "memory_mib": 1024, "root_disk_mib": 8192},
     "allowed_images": ["python:3.12"],
     "network": {
       "enabled": true,
       "max_tcp_connections": 64,
       "max_udp_connections": 64,
       "rules": [
         {"action": "allow", "destination": {"group": "public"}, "protocols": ["tcp"], "ports": [{"start": 443, "end": 443}]},
         {"action": "allow", "destination": {"group": "host"}, "protocols": ["tcp", "udp"], "ports": [{"start": 53, "end": 53}]}
       ]
     }
   }
   ```

   宛先を広く許可する場合は Worker 側の `rules` に `{"action":"allow","destination":{"any":true}}` を指定できます。metadata endpoint への拒否はこのルールより常に優先します。

## 結果と終了

各 `ResultOutputItem::Data` は、protobuf エンコードした `SandboxExecResult` です。stdout または stderr を示す `stream` と生バイト列を持つ `output`、終了コード・実行時間・VM 識別子を持つ最終 `exit` を含みます。出力は UTF-8 である必要はなく、行単位には分割されません。

| message | フィールドと型 | 内容 |
| --- | --- | --- |
| `SandboxExecResult` | `oneof { output: SandboxExecOutput, exit: SandboxExecExit }` | Data ごとに出力か最終終了結果の一方。 |
| `SandboxExecOutput` | `stream: string`, `data: bytes` | stream は `stdout` または `stderr`。PTY では stdout に併合。 |
| `SandboxExecExit` | `exit_code: int32`, `execution_time_ms: uint64`, `sandbox_id: string` | exit_code はプロセスの終了コード。sandbox_id は比較・記録用の不透明な VM 識別子。 |

非成功終了をエラーとして扱う場合でも、終了コードを取得できた場合は最終 `exit` の Data を返します。起動失敗や timeout のように終了コードを得られない場合は `exit` を作りません。stream を返す前の失敗は JobResult に反映されますが、**stream 開始後の失敗は次の `End.metadata` で通知**します。先に決まった JobResult の status が `Success` でも、コマンドが最後まで成功したとは限りません。`EnqueueForStream` / `EnqueueWithClientStream` の初期応答に入るのは JobResult ではなく job ID です。

### 実行完了と終端エラー

SANDBOX は `ResultOutputItem::End` の `metadata["stream_error"]` に実行後のエラーを入れます。値は **最大 4096 UTF-8 bytes の JSON 文字列**で、Runner 設定を protobuf bytes に変換する JSON とは異なります。長い診断文は SANDBOX が UTF-8 の文字境界で切り詰めます。例えば次の値は、終了コード 7 の `exit` Data に続くエラー End を表します。

```json
{"version":1,"code":"EXECUTION_FAILED","message":"Sandbox command exited with code 7","origin":"SANDBOX"}
```

| 項目 | 意味 |
| --- | --- |
| `version` | 形式の版。現在は整数の `1`。 |
| `code` | エラー分類。SANDBOX は `EXECUTION_FAILED`、`TIMEOUT`、`CANCELLED` を使用。未知の分類もエラーとして扱う。 |
| `message` | 表示用の診断文。コマンドの引数、stdin、env は含めない。 |
| `origin` | エラーを作成した Runner。SANDBOX では `SANDBOX`。 |

クライアントは最後まで stream を読み、**`End` があり、かつこのキーがない場合にだけ SANDBOX の実行完了を成功と判定**してください。キーがある場合は、その JSON を読んで失敗として扱います。JSON が不正・上限超過、`version` が非対応、または `End` を受け取る前に stream が途切れた場合も成功とみなさないでください。Data が先に届いていても取り消されません。クライアント切断などではエラー End の送達自体を保証できません。現行の gRPC 配信処理は `End.metadata` を gRPC のエラー status に自動変換しないため、gRPC 呼出し自体が正常終了した場合もこの確認が必要です。

このキーは将来ほかの Runner と共有できる形ですが、**現時点で LLM・WORKFLOW を含む全 Runner が同じ形式を返すわけではありません**。既存の Runner 固有の `End` キーも変更しません。WORKFLOW が SANDBOX を子ジョブとして実行する場合は、SANDBOX のエラー End、不正な `stream_error`、`End` 不在を子ジョブの失敗として扱います。この対応は、WORKFLOW 自体がすべての終端エラーを同じキーで返すことを意味しません。

クライアント切断、キャンセル、timeout 時は実行プロセスを終了します。non-static VM は削除し、static VM は再利用できない場合に破棄して次回実行時に作り直します。

`run_with_client` で stdin の書込みに失敗しても、それだけで stream は終了しません。残りの出力とプロセスの終了を待ち、最終的な実行結果を `End` で確認してください。キャンセルによる `CANCELLED` の通知も、クライアント切断などで `End` が届かない場合は確認できません。

## セキュリティ上の注意

- `use_static=false` は、相互に信頼しないジョブの既定モードです。
- bind mount、`host` profile、private 宛先の許可は、ホスト資産へのアクセスを与えます。Worker 管理者は image、mount、ネットワーク許可を最小限にしてください。
- Worker 更新・削除時の VM cleanup は非同期です。実行ホスト障害後に sandbox 状態が残る可能性があるため、運用者は孤立 VM を定期的に確認・削除してください。
- shutdown 時は稼働中の実行をキャンセルし、VM の停止・削除を試みます。all-in-one は既存の最大 5 秒で終了を待ち、worker 専用実行は既存どおり期限なしで待ちます。強制終了やホスト障害時は VM の削除を保証しません。

## 関連資料

- [jobworkerp-rs のストリーミング機能](../streaming.md)
- [microsandbox Execution](https://docs.microsandbox.dev/sdk/rust/execution)
- [microsandbox Rust Sandbox API（VM 資源・寿命・root disk）](https://docs.microsandbox.dev/sdk/rust/sandbox)
- [microsandbox Rust Networking API（ルール・接続数上限）](https://docs.microsandbox.dev/sdk/rust/networking)
- [microsandbox Lifecycle](https://docs.microsandbox.dev/sandboxes/lifecycle)
- [microsandbox Volumes](https://docs.microsandbox.dev/sandboxes/volumes)
- [microsandbox Networking](https://docs.microsandbox.dev/networking/overview)
- [microsandbox Network defenses（metadata・DNS rebinding）](https://docs.microsandbox.dev/security/network)
- [microsandbox Bootstrap（script と通常の実行の違い）](https://docs.microsandbox.dev/sandboxes/bootstrap)
