# for-task の最終 output が「ループ開始前の値」になる課題

## 概要

`ForTaskStreamExecutor` の最終 `TaskCompleted` イベントは `original_context.output`(=for-task の入力)をそのまま返す。これは下流の `execute_workflow` で `WorkflowContext.output` の最終値として上書きされるため、**for-task が完走しても output が「ループ開始前の値」になる**。失敗情報も反復結果も含まれない。

## 該当コード

- `app-wrapper/src/workflow/execute/task/stream/for.rs:564-588` (parallel)
- `app-wrapper/src/workflow/execute/task/stream/for.rs:786-799` (sequential)

両経路とも `original_context` をそのまま再利用している。

## 影響

- ワークフロー作成者が `${ . }` で for-task の出力を参照すると、想定外に「入力配列そのもの」が返る
- per-item の処理結果を後続タスクで集計する典型ユースケースが直感に反する
- `onError: continue` で失敗を握りつぶした場合、失敗があったかどうかも output からは分からない

## 既に対処済の関連項目

- per-iteration の失敗イベント `WorkflowStreamEvent::ForItemFailed` を導入し、`WorkflowContext.output` を上書きしないようにした(commit で `TaskCompleted("forTask")` 偽装の問題は解消済)。これにより「per-item の error が一旦 output に乗ったあと最終で巻き戻る」現象は発生しない。
- ただし「最終 output が input と同一」という根本仕様自体は未解決のまま残っている。

## 提案する解決方針

最終 `TaskCompleted("forTask")` イベントの output を **per-iteration 結果の配列** に置き換える:

```jsonc
{
  "results": [
    { "index": 0, "ok": <iter0 do の最終 output> },
    { "index": 1, "error": <error payload>, "item": <元 item> },
    { "index": 2, "ok": <iter2 do の最終 output> }
  ]
}
```

- parallel 経路: spawn task 内で `last_ctx.output` を `Arc<Mutex<Vec<(usize, Value)>>>` に push、`stream::once` の最終 cleanup で sort → `final_ctx.set_raw_output(...)`
- sequential 経路: ループ内で蓄積 → 最終 yield 時に set
- `onError: continue` の失敗 item は ForItemFailed の `error_payload` 相当を同 vec に push

## 影響範囲

- 既存テストで forTask の output を直接参照しているものは契約変更が必要
- 公開 DSL spec(Serverless Workflow)との整合: spec 仕様では for-task の output 動作は明確に規定されていない箇所があるため、本プロジェクト側で「per-iteration 集約」を選んでも独自拡張として問題ない見込み
- ただし下流で「forTask 後の output = 入力配列」を前提にした処理がある場合は破壊的

## 優先度

中。実害は出ているが、`onError: continue` を使う高度なケースが影響を強く受ける一方、典型的な `inParallel: false` + 全成功ケースでは output が input と同じことが許容される実装も存在しうるため、まずは下流影響調査を優先。
