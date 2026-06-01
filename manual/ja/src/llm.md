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

## ランナー設定

| フィールド | 説明 |
|-----------|------|
| `using` | 使用するメソッド: `"completion"` または `"chat"`。ジョブ実行時に`JobRequest.using`で指定（Runner Settingsのフィールドではない） |
| worker.runner_settings | モデル設定（プロバイダー、モデル名、パラメータ） |
| job.args | プロンプト（completion）またはメッセージ（chat） |

## 関連ドキュメント

- [Function / FunctionSet](function.md) - 統一的なFunction抽象層、FunctionSet管理、AutoSelection
- [ストリーミング](streaming.md) - LLMトークン生成のストリーミング実行
