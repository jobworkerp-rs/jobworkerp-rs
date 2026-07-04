# LLM Runner

## 概要

LLMランナーはマルチメソッドランナーとして、jobworkerp-rsを通じてLLM（大規模言語モデル）の機能を提供します。`using`パラメータで実行メソッドを指定します。

## メソッド

### completion

テキスト補完（プロンプトベース）。

- プロンプトをLLMに送信し、生成されたテキスト応答を受け取る
- シンプルなテキスト生成タスクに適している

### chat

チャット会話（メッセージ履歴付き、ツール呼び出し対応）。

- メッセージ履歴を使用したマルチターン会話をサポート
- FunctionSetを使用したツール呼び出し（後述の[ツール呼び出し](#ツール呼び出し)を参照）
- ストリーミングトークン生成をサポート（[ストリーミング](streaming.md)を参照）

### embedding

テキスト埋め込み生成（テキストのベクトル表現）。

- 1つ以上のテキスト入力を埋め込みベクトルに変換（セマンティック検索、RAG、クラスタリング等に利用）
- 長い入力は自動的にチャンク分割されます（後述の[テキストチャンキング](#テキストチャンキングembedding)を参照）。各チャンクは、元の入力インデックスと文字範囲が付与された個別のベクトルを生成します
- バッチ入力対応（1ジョブで複数テキスト）
- このメソッドはストリーミング**非対応**

詳細は後述の[埋め込みの利用方法](#埋め込みの利用方法)を参照してください。

## 対応するLLMプロバイダー

- **Ollama**: ローカルLLMサーバー、ツール呼び出し完全対応
- **GenAI**: マルチプロバイダーLLMクライアント（OpenAI、Anthropic、Gemini、Cohere、Groq、xAI、DeepSeek、Ollama等の各種バックエンド対応）。base_urlの設定によりOpenAI API互換サーバーも利用可能

### GenAIプロバイダーの設定方法

GenAIは[rust-genai](https://github.com/jeremychone/rust-genai)クレートを利用しています。プロバイダーはモデル名から自動検出されます（例: `gpt-*` → OpenAI, `claude-*` → Anthropic, `gemini-*` → Gemini）。認証は環境変数で行います — ワーカー起動前に適切なAPIキーを設定してください。

#### モデル名プレフィックスによるプロバイダー自動検出

プロバイダーはモデル名のプレフィックスから自動判定されます：

| プレフィックス | プロバイダー | 環境変数 | 例 |
|--------------|------------|---------|-----|
| `gpt*` | OpenAI | `OPENAI_API_KEY` | `gpt-4o` |
| `claude*` | Anthropic | `ANTHROPIC_API_KEY` | `claude-sonnet-4-20250514` |
| `gemini*` | Google Gemini | `GEMINI_API_KEY` | `gemini-2.5-flash` |
| `command*` | Cohere | `COHERE_API_KEY` | `command-r-plus` |
| `deepseek*` | DeepSeek | `DEEPSEEK_API_KEY` | `deepseek-chat` |
| `glm*` | ZAI | `ZAI_API_KEY` | `glm-4-plus` |
| (その他) | Ollama | *(不要 — ローカル)* | `llama3.3` |

プレフィックスから推測できないプロバイダーの場合、名前空間プレフィックス `provider::model-name` を使用します：

| 名前空間プレフィックス | プロバイダー | 環境変数 | 例 |
|---------------------|------------|---------|-----|
| `groq::` | Groq | `GROQ_API_KEY` | `groq::llama-3.3-70b-versatile` |
| `xai::` | xAI | `XAI_API_KEY` | `xai::grok-3-mini` |
| `open_router::` | OpenRouter | `OPEN_ROUTER_API_KEY` | `open_router::meta-llama/llama-3.3-70b-instruct` |
| `ollama_cloud::` | Ollama Cloud | `OLLAMA_API_KEY` | `ollama_cloud::llama3.3` |

> 最新の完全なマッピングルールは [rust-genai examples/c00-readme.rs](https://github.com/jeremychone/rust-genai/blob/main/examples/c00-readme.rs) を参照してください。

#### 環境変数の設定

利用するプロバイダーのAPIキーを`.env`ファイルまたはシステム環境変数に設定します：

```bash
# .env
OPENAI_API_KEY="sk-..."
ANTHROPIC_API_KEY="sk-ant-..."
GEMINI_API_KEY="AIza..."
```

#### カスタムエンドポイント / OpenAI API互換サーバーの利用

`GenaiRunnerSettings`の`base_url`にカスタムエンドポイントを指定します。vLLM、LocalAI等のセルフホスト型OpenAI互換サーバーやプロキシサービスを利用する場合に有用です。

```json
{
  "genai": {
    "model": "gpt-4o",
    "base_url": "https://your-proxy.example.com/v1/"
  }
}
```

パスが空または`/`の場合、自動的に`/v1/`に正規化されます。

> **注意:** LiteLLM等のOpenAI互換プロキシ経由で非OpenAIモデルを利用する場合、モデル名に`openai::`プレフィックスを付けてOpenAIアダプターを強制してください（例: `openai::claude-sonnet-4-20250514`）。プレフィックスがないとモデル名からプロバイダーが推測され、プロキシと互換性のないプロトコルが使用されます。

## ツール呼び出し

chatメソッドでは、FunctionSetを指定することでLLMにツールを提供できます。`is_auto_calling`オプションで自動/手動モードを切り替え可能です：

- `is_auto_calling: true` - LLMがツール呼び出しを返すと自動実行
- `is_auto_calling: false`（デフォルト）- ツール呼び出しをクライアントに返却し、クライアント側で確認・修正後に実行をリクエスト

### FunctionSet

ツールはFunctionSetにより機能別に整理されています。利用可能な組み込みファンクションセットはランナータイプに対応しています：
- COMMAND, HTTP_REQUEST, GRPC, DOCKER 等
- MCPサーバーツール

FunctionSetの定義・管理、およびAutoSelection（LLMによるFunctionSetの自動選択でコンテキスト使用量を削減）の詳細は、[Function / FunctionSet](function.md)を参照してください。

## 埋め込みの利用方法

`embedding`メソッドは埋め込みベクトルを生成します。ジョブ実行時に`using`を`"embedding"`に設定します。ランナー設定（プロバイダー／モデル）は`completion`/`chat`と共通です。埋め込み対応モデルを指定してください。

### 埋め込み対応モデル

| プロバイダー | モデル例 | APIキー |
|------------|---------|---------|
| Ollama | `nomic-embed-text`, `mxbai-embed-large` | *(不要 — ローカル)* |
| OpenAI (GenAI) | `text-embedding-3-small`, `text-embedding-3-large` | `OPENAI_API_KEY` |
| Cohere (GenAI) | `embed-english-v3.0`, `embed-multilingual-v3.0` | `COHERE_API_KEY` |
| Gemini (GenAI) | `text-embedding-004`, `gemini-embedding-001` | `GEMINI_API_KEY` |

プロバイダー自動検出と環境変数の設定は completion/chat と共通です（[GenAIプロバイダーの設定方法](#genaiプロバイダーの設定方法)を参照）。

### ジョブ引数（`job.args`）

| フィールド | 説明 |
|-----------|------|
| `inputs` | 入力バッチ（最低1つ）。各入力は`text`を持ちます。image/media バリアントは将来のマルチモーダル対応用に予約されており、現状は未対応エラーを返します。 |
| `model` | 設定モデルのジョブ単位オーバーライド（任意）。 |
| `options` | 埋め込みオプション（任意、下記参照）。 |
| `chunking` | チャンキングのジョブ単位オーバーライド（任意）。未設定時は設定レベルの`embedding_chunking`を使用。 |

#### `options`

| オプション | 説明 | プロバイダー |
|-----------|------|------------|
| `dimensions` | 出力次元数（モデルが対応する場合）。 | GenAI (OpenAI/Cohere/Gemini), Ollama |
| `truncate` | 長すぎる入力の扱い: `"NONE"` / `"START"` / `"END"`。GenAI (Cohere) はそのまま転送。Ollama はブール値にマッピング（`NONE`→false, `START`/`END`→true。Ollama には START/END の区別なし）。未設定 → プロバイダー既定。 | GenAI (Cohere), Ollama |
| `embedding_type` | 用途/種別: `"search_document"`/`"search_query"` (Cohere) または `"RETRIEVAL_DOCUMENT"`/`"RETRIEVAL_QUERY"` (Gemini)。Ollama は無視。 | GenAI (Cohere/Gemini) |
| `encoding_format` | 出力ベクトルのエンコーディング。float 系の値（`"float"`/`"float32"`）のみ受け付けます — ベクトルは常に`f32`にデコードされるため、非 float（base64/binary/int8/…）は警告を出して拒否され、プロバイダー既定（float）が使われます。Ollama は無視。 | GenAI (OpenAI/Cohere) |
| `user` | 悪用検知・レート管理用のエンドユーザー識別子（OpenAI `user`）。Cohere/Gemini/Ollama は無視。 | GenAI (OpenAI) |

### 結果（`LLMEmbeddingResult`）

- `embeddings`: テキストチャンク（または非テキスト入力）ごとに1エントリ。入力インデックス順→チャンク順に並びます。各エントリは以下を持ちます:
  - `vector`: 埋め込みベクトル（`repeated float`）
  - `input_index`: リクエスト`inputs`への0起点インデックス
  - `begin_position` / `end_position`: 入力テキスト内の対象部分文字列の半開区間の文字範囲`[begin, end)`（バイトオフセットではなく Unicode スカラーインデックス）
  - `content`: このチャンクが対象とする部分文字列そのもの
  - `dimensions`: `vector`の長さ
- `usage`（任意）: `model`, `prompt_tokens`, `total_tokens`。Ollama はトークン数を返しません（モデル名のみ）。GenAI はプロバイダーが usage を返す場合にトークン欄を埋めます。トークン数はリトライバッチ間で合算されます。

### テキストチャンキング（embedding）

長い入力は各チャンクがモデルのコンテキスト予算内に収まるよう分割されます。チャンキングはランナー設定の`embedding_chunking`（全ジョブの既定）または`job.args`の`chunking`（ジョブ単位オーバーライド）で設定します。

| フィールド | 説明 | 既定値 |
|-----------|------|-------|
| `max_chunk_tokens` | チャンクあたりのトークン数上限（> 0 必須）。 | 512 |
| `min_chunk_tokens` | 小さな隣接チャンクをマージする際の下限（`max_chunk_tokens`未満に保たれる）。 | 0 |
| `token_estimation` | トークン長の推定方法: `CHARACTER_ESTIMATION`（1、約4文字/トークン、トークナイザー不要）、`TIKTOKEN`（2、OpenAI BPE）、`HF_TOKENIZER`（3、HuggingFace）。未指定 → 文字数推定。 | 文字数推定 |
| `tiktoken_encoding` | `token_estimation = TIKTOKEN`時の tiktoken エンコーディング: `"cl100k_base"`（text-embedding-3 / GPT-3.5/4）または`"o200k_base"`（GPT-4o）。 | `cl100k_base` |
| `tokenizer_hf_repo` | `token_estimation = HF_TOKENIZER`時に`tokenizer.json`を取得する HuggingFace repo id（例: `"nomic-ai/nomic-embed-text-v1.5"`）。hf-hub 経由でダウンロード/キャッシュ。Ollama モデル名は HF repo に**自動マッピングされません** — repo を明示指定してください。 | — |
| `tokenizer_file_path` | `HF_TOKENIZER`用のローカル`tokenizer.json`の絶対パス（オフライン用）。`tokenizer_hf_repo`より優先。 | — |

補足:
- **TIKTOKEN** は OpenAI/GenAI モデル向けに正確なトークン数を提供します。文字境界は文字列検索にフォールバックします（文字数推定と同等の精度）。
- **HF_TOKENIZER** は対応する HuggingFace トークナイザーが判明している Ollama モデル向けです。正確なトークン数と文字範囲を生成します。private repo は環境に`HF_TOKEN`が必要です（public repo はキー不要）。
- リクエスト時にバッチがモデルのコンテキスト長を超えた場合、ランナーは自動的に`max_chunk_tokens`を縮小して再チャンクし、最終的に単一アイテムずつのリクエストにフォールバックします。

### 使用例（gRPC / jobworkerp-client）

ワーカー設定（Ollama、埋め込みモデル）:

```json
{
  "ollama": {
    "base_url": "http://localhost:11434",
    "model": "nomic-embed-text"
  },
  "embedding_chunking": {
    "max_chunk_tokens": 512,
    "token_estimation": "HF_TOKENIZER",
    "tokenizer_hf_repo": "nomic-ai/nomic-embed-text-v1.5"
  }
}
```

ジョブ引数（`using = "embedding"`）:

```json
{
  "inputs": [
    { "text": "The quick brown fox jumps over the lazy dog." },
    { "text": "jobworkerp-rs is a scalable job worker system." }
  ],
  "options": { "dimensions": 768 }
}
```

OpenAI の例（ワーカー設定 + 引数）:

```json
{ "genai": { "model": "text-embedding-3-small" } }
```

```json
{
  "inputs": [{ "text": "Semantic search query." }],
  "options": { "dimensions": 256, "encoding_format": "float", "user": "tenant-42" },
  "chunking": { "max_chunk_tokens": 256, "token_estimation": "TIKTOKEN", "tiktoken_encoding": "cl100k_base" }
}
```

## ランナー設定

| フィールド | 説明 |
|-----------|------|
| `using` | 使用するメソッド: `"completion"`、`"chat"`、または`"embedding"`。ジョブ実行時に`JobRequest.using`で指定（Runner Settingsのフィールドではない） |
| worker.runner_settings | モデル設定（プロバイダー、モデル名、パラメータ）。embedding では`embedding_chunking`も指定 |
| job.args | プロンプト（completion）、メッセージ（chat）、または入力（embedding） |

## 関連ドキュメント

- [Function / FunctionSet](function.md) - 統一的なFunction抽象層、FunctionSet管理、AutoSelection
- [ストリーミング](streaming.md) - LLMトークン生成のストリーミング実行
