use agent_server::approval::{
    MemoryPendingApprovalStore, PendingApproval, PendingApprovalStore, RedisPendingApprovalStore,
};
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn pending(chat_id: &str, call_id: &str) -> PendingApproval {
    PendingApproval {
        chat_id: chat_id.to_owned(),
        call_id: call_id.to_owned(),
        tool_name: "deploy".to_owned(),
        worker_id: 27,
        method: "run".to_owned(),
        arguments: json!({"environment": "staging"}),
        continuation_history: vec![json!({"role": "assistant", "content": "continue"})],
        deadline: Utc::now() + ChronoDuration::minutes(1),
    }
}

#[tokio::test]
async fn memory_store_saves_all_pending_approval_context() {
    let store = MemoryPendingApprovalStore::new(Duration::from_secs(30));
    let expected = pending("chat-1", "call-1");

    store.save(expected.clone()).await.unwrap();

    assert_eq!(
        store.claim("chat-1", "call-1").await.unwrap(),
        Some(expected)
    );
}

#[tokio::test]
async fn memory_store_allows_only_one_concurrent_claim() {
    let store = Arc::new(MemoryPendingApprovalStore::new(Duration::from_secs(30)));
    store.save(pending("chat-2", "call-2")).await.unwrap();
    let mut claimers = Vec::new();

    for _ in 0..16 {
        let store = Arc::clone(&store);
        claimers.push(tokio::spawn(async move {
            store.claim("chat-2", "call-2").await.unwrap()
        }));
    }

    let mut successful_claims = 0;
    for claimer in claimers {
        successful_claims += usize::from(claimer.await.unwrap().is_some());
    }
    assert_eq!(successful_claims, 1);
}

#[tokio::test]
async fn memory_store_expires_pending_approvals_after_ttl() {
    let store = MemoryPendingApprovalStore::new(Duration::from_millis(20));
    store.save(pending("chat-3", "call-3")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;

    assert_eq!(store.claim("chat-3", "call-3").await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires a Redis service configured for infra-utils tests"]
async fn redis_store_allows_only_one_claim() {
    let url =
        std::env::var("TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_owned());
    let client = redis::Client::open(url).unwrap();
    let store = Arc::new(RedisPendingApprovalStore::new(
        client,
        Duration::from_secs(30),
    ));
    let item = pending("redis-chat", &uuid::Uuid::new_v4().to_string());
    store.save(item.clone()).await.unwrap();
    let call_id = item.call_id.clone();

    let first = {
        let store = Arc::clone(&store);
        let call_id = call_id.clone();
        tokio::spawn(async move { store.claim("redis-chat", &call_id).await.unwrap() })
    };
    let second = {
        let store = Arc::clone(&store);
        tokio::spawn(async move { store.claim("redis-chat", &call_id).await.unwrap() })
    };

    assert_eq!(
        usize::from(first.await.unwrap().is_some()) + usize::from(second.await.unwrap().is_some()),
        1
    );
}
