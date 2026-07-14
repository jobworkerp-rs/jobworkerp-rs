# Docker環境での実行

## docker-composeによる起動

```shell
# docker-compose で起動
$ docker-compose up

# スケーラブル構成で起動（本番環境向け）
$ docker-compose -f docker-compose-scalable.yml up --scale jobworkerp-worker=3
```

## ローカルでのDockerイメージビルド

all-in-oneイメージ用に2つのDockerfileがあります：

| Dockerfile | 用途 | 説明 |
|------------|------|------|
| `Dockerfile` | CI / デモ配布用 | 事前にビルドされた `./target/release/all-in-one` が必要。GitHub Actionsでのイメージ公開やビルド済みバイナリがある環境向け。 |
| `Dockerfile.full` | ローカル開発・テスト用 | フルマルチステージビルド。RustバイナリとAdmin UIの両方をDocker内でビルド。事前ビルド不要。 |

```shell
# オプション1: ビルド済みバイナリを使用 (Dockerfile)
# まずRustバイナリをローカルでビルド
$ cargo build --release
# Dockerイメージをビルド
$ docker build -t jobworkerp-all-in-one .

# オプション2: Docker内でフルビルド (Dockerfile.full)
# 事前ビルド不要 - すべてDocker内でビルドされます
$ docker build -f Dockerfile.full -t jobworkerp-all-in-one .

# コンテナを実行
# - ポート8080: Admin UI (nginx + MCP/AG-UIリバースプロキシ)
# - ポート9000: gRPC / gRPC-Web
# - ポート8000: MCP Server (直接アクセス)
# - ポート8001: AG-UI Server (直接アクセス)
# - docker.sock: ホストのDockerデーモンをマウント (DOCKERランナー用)
$ docker run -p 8080:8080 -p 9000:9000 -p 8000:8000 -p 8001:8001 \
  -v /var/run/docker.sock:/var/run/docker.sock \
  jobworkerp-all-in-one
```

### worker デプロイ時の Docker ソケット権限

`worker-main/Dockerfile` の worker は非 root ユーザー `jobworkerp` として
実行されます。スケーラブル Compose 構成は、ホストの Docker ソケットの数値
GID を worker の補助グループとして追加します。起動前に以下を実行してください。

```shell
export DOCKER_SOCKET_GID="$(stat -c '%g' /var/run/docker.sock)"
docker-compose -f docker-compose-scalable.yml up --scale jobworkerp-worker=3
```

`DOCKER_SOCKET_GID` は `compose-scalable.env` ではなく Compose を起動するシェルで設定します。
ソケットを world-writable に変更しないでください。

gRPC を公開する環境では `AUTH_TOKEN` を設定してください。保護対象の gRPC リクエストでは metadata header として `jobworkerp-auth: <AUTH_TOKEN>` を送信します。**Admin UI を使う場合は `AUTH_TOKEN` を設定しないでください**: 同梱の Admin UI クライアントはこのヘッダをまだ送信しないため、保護対象の呼び出し（ジョブ投入など）が `Unauthenticated` で失敗します。Admin UI が認証ヘッダに対応するまでは `AUTH_TOKEN` を未設定のままにしてください。

## 起動後のアクセス

コンテナ起動後、ブラウザで [http://localhost:8080](http://localhost:8080) にアクセスするとAdmin UI（管理画面）が利用できます。

## 分離workerイメージのビルドとテスト公開

`worker-main/Dockerfile` は Ubuntu 24.04 の builder stage 内で worker をビルドします。ホストで作成した `target/release/worker` はイメージに含まれません。

1. MySQL 構成の worker イメージをビルドします。

   ```shell
   docker build -f worker-main/Dockerfile \
     --build-arg CARGO_FEATURES=mysql \
     -t gitea.sutr.app/jobworkerp-rs/worker:test-<commit-sha> .
   ```

2. Gitea Package Registry にログインしてテストタグを公開します。

   ```shell
   docker login gitea.sutr.app
   docker push gitea.sutr.app/jobworkerp-rs/worker:test-<commit-sha>
   ```

3. デプロイ対象の worker image をこのタグへ更新し、worker の起動ログと job 実行を確認します。
管理画面ではWorkerの作成・管理、ジョブの投入・監視などの操作をGUIで行えます。

### ポート構成

| ポート | サービス | アクセスURL |
|--------|----------|-------------|
| 8080 | Admin UI (nginx) | `http://localhost:8080` |
| 9000 | gRPC / gRPC-Web | `localhost:9000` |
| 8000 | MCP Server | `http://localhost:8000/mcp` |
| 8001 | AG-UI Server | `http://localhost:8001/ag-ui/run` |

nginx経由でのプロキシアクセスも可能: `http://localhost:8080/mcp`, `http://localhost:8080/ag-ui/`
