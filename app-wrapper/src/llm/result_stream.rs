//! Result-stream adapters shared by the LLM runners. Every stream they build
//! ends with exactly one End item, so consumers can tell success, provider
//! failure and cancellation apart.

use std::collections::HashMap;

use futures::stream::{BoxStream, Stream, StreamExt};
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamErrorCode, result_output_item};
use tokio_util::sync::CancellationToken;

/// Message for a provider stream that broke off. Provider error details stay
/// in the logs.
pub(crate) const PROVIDER_STREAM_INTERRUPTED: &str = "LLM provider stream interrupted";

/// Error End attributed to the LLM runner.
pub(crate) fn llm_error_end(
    metadata: HashMap<String, String>,
    code: StreamErrorCode,
    message: &str,
) -> ResultOutputItem {
    proto::stream_error::error_end_item(metadata, code, message, RunnerType::Llm.as_str_name())
}

/// Serializes typed provider results for which `should_emit` holds.
///
/// The stream ends normally after the first result for which `is_done`
/// holds. A provider stream that stops before that point is a provider
/// failure, and cancellation ends it with CANCELLED. `keep_alive` is held
/// until the stream finishes (e.g. the root tracing span context).
pub(crate) fn provider_result_stream<T, S, K>(
    results: S,
    metadata: HashMap<String, String>,
    cancel_token: CancellationToken,
    keep_alive: K,
    should_emit: fn(&T) -> bool,
    is_done: fn(&T) -> bool,
) -> BoxStream<'static, ResultOutputItem>
where
    T: prost::Message + 'static,
    S: Stream<Item = T> + Send + 'static,
    K: Send + 'static,
{
    async_stream::stream! {
        let _keep_alive = keep_alive;
        futures::pin_mut!(results);
        loop {
            tokio::select! {
                result = results.next() => match result {
                    Some(result) => {
                        if should_emit(&result) {
                            yield ResultOutputItem {
                                item: Some(result_output_item::Item::Data(result.encode_to_vec())),
                            };
                        }
                        if is_done(&result) {
                            yield proto::stream_error::normal_end_item(metadata);
                            return;
                        }
                    }
                    None => {
                        tracing::error!("LLM provider stream ended before the terminal result");
                        yield llm_error_end(
                            metadata,
                            StreamErrorCode::ProviderStreamFailed,
                            "LLM provider stream ended before the terminal result",
                        );
                        return;
                    }
                },
                _ = cancel_token.cancelled() => {
                    tracing::info!("LLM stream was cancelled");
                    yield llm_error_end(metadata, StreamErrorCode::Cancelled, "LLM stream was cancelled");
                    return;
                }
            }
        }
    }
    .boxed()
}

/// Forwards an already-terminated result stream and ends it with CANCELLED
/// when the job is cancelled before the inner End arrives.
pub(crate) fn cancellable_result_stream<S, K>(
    items: S,
    metadata: HashMap<String, String>,
    cancel_token: CancellationToken,
    keep_alive: K,
) -> BoxStream<'static, ResultOutputItem>
where
    S: Stream<Item = ResultOutputItem> + Send + 'static,
    K: Send + 'static,
{
    async_stream::stream! {
        let _keep_alive = keep_alive;
        futures::pin_mut!(items);
        loop {
            tokio::select! {
                item = items.next() => match item {
                    Some(item) => {
                        let is_end = matches!(item.item, Some(result_output_item::Item::End(_)));
                        yield item;
                        if is_end {
                            return;
                        }
                    }
                    None => return,
                },
                _ = cancel_token.cancelled() => {
                    tracing::info!("LLM stream was cancelled");
                    yield llm_error_end(metadata, StreamErrorCode::Cancelled, "LLM stream was cancelled");
                    return;
                }
            }
        }
    }
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobworkerp_runner::jobworkerp::runner::llm::LlmChatResult;
    use proto::jobworkerp::data::Trailer;

    fn chat(done: bool) -> LlmChatResult {
        LlmChatResult {
            done,
            ..Default::default()
        }
    }

    fn last_end(items: &[ResultOutputItem]) -> &Trailer {
        let ends: Vec<_> = items
            .iter()
            .filter_map(|item| match &item.item {
                Some(result_output_item::Item::End(trailer)) => Some(trailer),
                _ => None,
            })
            .collect();
        assert_eq!(ends.len(), 1, "exactly one End: {items:?}");
        assert!(matches!(
            items.last().unwrap().item,
            Some(result_output_item::Item::End(_))
        ));
        ends[0]
    }

    fn error_code(trailer: &Trailer) -> Option<i32> {
        trailer.error.as_ref().map(|error| error.code)
    }

    async fn provider_items(
        results: Vec<LlmChatResult>,
        token: CancellationToken,
    ) -> Vec<ResultOutputItem> {
        provider_result_stream(
            futures::stream::iter(results),
            HashMap::new(),
            token,
            (),
            |_| true,
            |r: &LlmChatResult| r.done,
        )
        .collect()
        .await
    }

    #[tokio::test]
    async fn provider_stream_ends_normally_after_the_done_result() {
        let items = provider_items(vec![chat(false), chat(true)], CancellationToken::new()).await;
        assert_eq!(items.len(), 3);
        assert_eq!(error_code(last_end(&items)), None);
    }

    #[tokio::test]
    async fn provider_stream_without_done_is_a_provider_failure() {
        let items = provider_items(vec![chat(false)], CancellationToken::new()).await;
        assert_eq!(
            error_code(last_end(&items)),
            Some(StreamErrorCode::ProviderStreamFailed as i32)
        );
    }

    #[tokio::test]
    async fn cancelled_streams_end_with_cancelled() {
        let token = CancellationToken::new();
        token.cancel();
        let pending = futures::stream::pending::<LlmChatResult>();
        let items: Vec<_> = provider_result_stream(
            pending,
            HashMap::new(),
            token.clone(),
            (),
            |_| true,
            |r: &LlmChatResult| r.done,
        )
        .collect()
        .await;
        assert_eq!(
            error_code(last_end(&items)),
            Some(StreamErrorCode::Cancelled as i32)
        );

        let items: Vec<_> = cancellable_result_stream(
            futures::stream::pending::<ResultOutputItem>(),
            HashMap::new(),
            token,
            (),
        )
        .collect()
        .await;
        assert_eq!(
            error_code(last_end(&items)),
            Some(StreamErrorCode::Cancelled as i32)
        );
    }

    #[tokio::test]
    async fn cancellable_stream_forwards_the_inner_end() {
        let items: Vec<_> = cancellable_result_stream(
            futures::stream::iter([proto::stream_error::normal_end_item(HashMap::new())]),
            HashMap::new(),
            CancellationToken::new(),
            (),
        )
        .collect()
        .await;
        assert_eq!(error_code(last_end(&items)), None);
    }
}
