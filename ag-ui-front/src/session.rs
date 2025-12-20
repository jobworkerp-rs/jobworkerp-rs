//! Session management for AG-UI connections.

pub mod manager;
pub mod redis_manager;
pub mod redis_store;
pub mod store;

// Re-export main types
pub use manager::{
    HitlWaitingInfo, InMemorySessionManager, PendingToolCallInfo, Session, SessionManager,
    SessionState,
};
pub use redis_manager::RedisSessionManager;
pub use redis_store::RedisEventStore;
pub use store::{EventStore, InMemoryEventStore};
