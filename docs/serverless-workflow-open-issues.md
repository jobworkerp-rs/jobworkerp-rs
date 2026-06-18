# Serverless Workflow 未解決課題

## call/run task の running job 登録レース

- Severity: Low
- Affected files: `app-wrapper/src/workflow/execute/task/call.rs`, `app-wrapper/src/workflow/execute/task/run.rs`
- Problem:
  `enqueue_with_worker_or_temp_channel` が child `JobId` を返してから `WorkflowContext::register_running_job` に登録するまでに短い隙間がある。この間に workflow cancellation が発生すると、child job が cancellation propagation の対象から漏れる可能性がある。また、result 待機中に task future が drop された場合、`unregister_running_job` が実行されない可能性がある。
- Current status:
  これは `call: http` 固有ではなく、既存の `RunTaskExecutor` と同じパターンである。`CallTaskExecutor` は cancellation 挙動を既存 run task と揃えるため、現時点では同じ実装方針を維持している。
- Proposed solution:
  child job 用の共通 guard を導入し、enqueue 直後に登録し、`Drop` または async cleanup 経路で解除する。`RunTaskExecutor` と `CallTaskExecutor` の両方から利用し、enqueue から result 完了までの間に cancellation が発生するケースのテストを追加する。
