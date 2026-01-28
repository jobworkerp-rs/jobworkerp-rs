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
- ストリーミングトークン生成をサポート（[STREAMING_ja.md](STREAMING_ja.md)を参照）

## 対応するLLMプロバイダー

- **Ollama**: ローカルLLMサーバー、ツール呼び出し完全対応
- **OpenAI API互換サーバー**: OpenAI APIインターフェースを実装する任意のサーバー
- **GenAI**: マルチプロバイダーLLMクライアント（Ollama等の各種バックエンド対応）

## ツール呼び出し

chatメソッドでは、FunctionSetを指定することでLLMにツールを提供できます。`is_auto_calling`オプションで自動/手動モードを切り替え可能です：

- `is_auto_calling: true` - LLMがツール呼び出しを返すと自動実行
- `is_auto_calling: false`（デフォルト）- ツール呼び出しをクライアントに返却し、クライアント側で確認・修正後に実行をリクエスト

### FunctionSet

ツールはFunctionSetにより機能別に整理されています。利用可能な組み込みファンクションセットはランナータイプに対応しています：
- COMMAND, HTTP_REQUEST, GRPC_UNARY, DOCKER 等
- MCPサーバーツール

## ランナー設定

| フィールド | 説明 |
|-----------|------|
| `using` | 使用するメソッド: `"completion"` または `"chat"` |
| worker.runner_settings | モデル設定（プロバイダー、モデル名、パラメータ） |
| job.args | プロンプト（completion）またはメッセージ（chat） |

## 非推奨ランナー

`LLM_COMPLETION`と`LLM_CHAT`は非推奨です。代わりに`LLM`ランナーの`using`パラメータを使用してください。

## 関連ドキュメント

- [ストリーミング](STREAMING_ja.md) - LLMトークン生成のストリーミング実行
