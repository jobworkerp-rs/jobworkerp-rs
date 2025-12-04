# MistralRS Runner Plugin

MistralRS を使用したローカルLLM推論プラグイン。jobworkerp-rs の `MultiMethodPluginRunner` として動作し、Chat completion と Text completion をサポートします。

## 機能

- **Chat Completion**: Tool calling サポート付きの対話型推論
- **Text Completion**: シンプルなテキスト生成
- **ストリーミング**: 両モードでストリーミング出力をサポート
- **Tool Calling**: gRPC FunctionService 経由でのツール実行（並列/逐次）
- **キャンセル**: 実行中のジョブをキャンセル可能

## ビルド

### 前提条件

- Rust 1.75+
- protobuf compiler (`protoc`)
- MistralRS のビルド要件（プラットフォームにより異なる）

### 基本ビルド（CPU のみ）

```bash
cd plugins/mistral_runner
cargo build --release
```

### GPU サポート付きビルド

```bash
# CUDA (NVIDIA GPU)
cargo build --release --features cuda

# CUDA + cuDNN
cargo build --release --features cudnn

# Metal (Apple Silicon)
cargo build --release --features metal

# Intel MKL
cargo build --release --features mkl

# Apple Accelerate
cargo build --release --features accelerate
```

ビルド成果物: `target/release/libplugin_runner_mistral.so` (Linux) / `.dylib` (macOS) / `.dll` (Windows)

## インストール

1. ビルドした `.so` ファイルを jobworkerp のプラグインディレクトリにコピー:

```bash
cp target/release/libplugin_runner_mistral.so /path/to/plugins/
```

2. 環境変数 `PLUGINS_RUNNER_DIR` でプラグインディレクトリを指定:

```bash
export PLUGINS_RUNNER_DIR=/path/to/plugins
```

3. jobworkerp を起動すると、プラグインが自動的にロードされます。

## 環境変数

### 必須

なし（全てオプション）

### Tool Calling 設定

| 変数名 | 説明 | デフォルト |
|--------|------|------------|
| `MISTRAL_GRPC_ENDPOINT` | FunctionService gRPC エンドポイント（例: `http://localhost:50051`） | なし（Tool calling 無効） |
| `MISTRAL_MAX_ITERATIONS` | Tool calling 最大反復回数 | 3 |
| `MISTRAL_TOOL_TIMEOUT_SEC` | 各 Tool 実行のタイムアウト（秒） | 30 |
| `MISTRAL_PARALLEL_TOOLS` | Tool の並列実行を有効化 | true |
| `MISTRAL_CONNECTION_TIMEOUT_SEC` | gRPC 接続タイムアウト（秒） | 10 |

### トレーシング設定

| 変数名 | 説明 | デフォルト |
|--------|------|------------|
| `OTLP_ADDR` | OpenTelemetry コレクターアドレス | なし（標準出力のみ） |
| `RUST_LOG` | ログレベル | info |

## 使用方法

### Runner 登録

プラグインは起動時に `MistralLocalLLM` という名前で登録されます。

### Worker 設定例

```json
{
  "name": "mistral-chat-worker",
  "runner_type": "PLUGIN",
  "runner_name": "MistralLocalLLM",
  "runner_settings": {
    "model_settings": {
      "gguf_model": {
        "model_name_or_path": "/path/to/models",
        "gguf_files": ["model.gguf"],
        "with_logging": true
      }
    }
  }
}
```

### Job 実行例

#### Chat Completion

```json
{
  "worker_name": "mistral-chat-worker",
  "args": {
    "messages": [
      {"role": "USER", "content": {"text": "Hello, how are you?"}}
    ],
    "options": {
      "max_tokens": 1024,
      "temperature": 0.7
    }
  },
  "using": "chat"
}
```

#### Text Completion

```json
{
  "worker_name": "mistral-chat-worker",
  "args": {
    "prompt": "Once upon a time",
    "options": {
      "max_tokens": 512
    }
  },
  "using": "completion"
}
```

### Tool Calling 使用例

1. `MISTRAL_GRPC_ENDPOINT` を設定
2. FunctionSet を登録
3. `function_options` を指定して Chat を実行:

```json
{
  "messages": [...],
  "function_options": {
    "use_function_calling": true,
    "function_set_name": "my-tools"
  }
}
```

## サポートするモデル形式

### Text Model

HuggingFace Hub からのモデル自動ダウンロード:

```json
{
  "text_model": {
    "model_name_or_path": "mistralai/Mistral-7B-v0.1",
    "isq_type": "Q4K",
    "with_logging": true,
    "with_paged_attn": true
  }
}
```

### GGUF Model

ローカルの GGUF ファイルを使用:

```json
{
  "gguf_model": {
    "model_name_or_path": "/path/to/models",
    "gguf_files": ["model-q4_k_m.gguf"],
    "tok_model_id": "mistralai/Mistral-7B-v0.1",
    "with_logging": true
  }
}
```

## 量子化タイプ (ISQ)

Text Model で使用可能な量子化タイプ:

| タイプ | 説明 |
|--------|------|
| Q4_0, Q4_1 | 4-bit 量子化 |
| Q5_0, Q5_1 | 5-bit 量子化 |
| Q8_0, Q8_1 | 8-bit 量子化 |
| Q2K - Q8K | K-quant 系列 |
| HQQ4, HQQ8 | Half-precision quantization |
| F8E4M3 | FP8 形式 |

## 制限事項

1. **推論中断不可**: MistralRS の推論は途中で中断できません。キャンセルは推論完了後にチェックされます。
2. **メモリ残留**: libloading の制約により、プラグインアンロード後もメモリに残ります。
3. **Tool Calling 必須設定**: Tool calling を使用するには `MISTRAL_GRPC_ENDPOINT` の設定が必須です。

## トラブルシューティング

### プラグインがロードされない

```bash
# ログレベルを上げて確認
RUST_LOG=debug ./all-in-one
```

### Tool calling が動作しない

1. `MISTRAL_GRPC_ENDPOINT` が正しく設定されているか確認
2. grpc-front が起動しているか確認
3. FunctionSet が登録されているか確認

### メモリ不足エラー

- GGUF モデルの使用を検討（メモリ効率が良い）
- より小さい量子化タイプを使用
- `auto_device_map` の設定を調整

## 関連ドキュメント

- [要求仕様書](../../docs/mistral-plugin-extraction-spec.md)
- [詳細設計書](../../docs/mistral-plugin-detailed-design.md)
- [進捗報告書](../../docs/mistral-plugin-progress-report.md)

## ライセンス

jobworkerp-rs プロジェクトのライセンスに準拠します。
