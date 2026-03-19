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
