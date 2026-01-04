//! Embedded AG-UI server for all-in-one deployment.
//!
//! This module provides functions to boot the AG-UI server as part of
//! an all-in-one deployment alongside other services (gRPC, MCP, etc.).

use crate::handler::AgUiHandler;
use crate::server::http::boot_ag_ui_server;
use crate::{
    InMemoryEventStore, InMemorySessionManager, RedisEventStore, RedisSessionManager, ServerConfig,
};
use anyhow::Result;
use app::module::AppModule;
use app_wrapper::modules::AppWrapperModule;
use command_utils::util::shutdown::ShutdownLock;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::watch;

/// Boot AG-UI server for embedded/all-in-one deployment.
///
/// This function handles all the setup logic internally, including:
/// - Storage type detection (Standalone/Scalable)
/// - Session manager and event store initialization
/// - Handler creation
/// - Graceful shutdown coordination
///
/// # Arguments
///
/// * `app_wrapper_module` - App wrapper module for workflow execution
/// * `app_module` - App module for job execution
/// * `lock` - Shutdown lock for coordinated shutdown
/// * `shutdown_recv` - Watch receiver for shutdown signal
///
/// # Environment Variables
///
/// * `STORAGE_TYPE` - Storage type: "Standalone" (in-memory) or "Scalable" (Redis)
/// * `AG_UI_ADDR` - Server bind address (default: 127.0.0.1:8001)
/// * `AG_UI_AUTH_ENABLED` - Enable authentication (default: false)
/// * `AG_UI_AUTH_TOKENS` - Valid tokens, comma-separated
/// * `AG_UI_REQUEST_TIMEOUT_SEC` - Request timeout (default: 60)
/// * `AG_UI_SESSION_TTL_SEC` - Session TTL (default: 3600)
///
/// # Example
///
/// ```ignore
/// use ag_ui_front::server::boot_embedded_server;
/// use tokio::sync::watch;
///
/// let (shutdown_send, shutdown_recv) = watch::channel(false);
/// let ag_ui_future = boot_embedded_server(
///     app_wrapper_module,
///     app_module,
///     lock.clone(),
///     shutdown_recv,
/// );
///
/// // Later, to shutdown:
/// shutdown_send.send(true)?;
/// ```
pub async fn boot_embedded_server(
    app_wrapper_module: Arc<AppWrapperModule>,
    app_module: Arc<AppModule>,
    lock: ShutdownLock,
    mut shutdown_recv: watch::Receiver<bool>,
) -> Result<()> {
    let server_config = ServerConfig::from_env();
    let ag_ui_config = server_config.ag_ui.clone();

    tracing::info!(
        "AG-UI configuration: bind_addr={}, auth_enabled={}, timeout={}s, session_ttl={}s",
        server_config.bind_addr,
        server_config.auth.enabled,
        ag_ui_config.timeout_sec,
        ag_ui_config.session_ttl_sec
    );

    // Create shutdown signal from watch receiver
    let shutdown_signal: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
        let _ = shutdown_recv.changed().await;
        tracing::info!("AG-UI server received shutdown signal");
    });

    // Determine storage type from environment variable
    let storage_type = std::env::var("STORAGE_TYPE")
        .unwrap_or_else(|_| "Standalone".to_string())
        .to_lowercase();

    // boot_ag_ui_server handles lock.unlock() internally, so we don't unlock here
    match storage_type.as_str() {
        "scalable" => {
            tracing::info!("AG-UI: Using Scalable mode with Redis session/event storage");
            let redis_pool = app_module
                .repositories
                .redis_module
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Redis module required for Scalable mode"))?
                .redis_pool;

            let session_manager = Arc::new(RedisSessionManager::new(
                redis_pool,
                ag_ui_config.session_ttl_sec,
            ));
            let event_store = Arc::new(RedisEventStore::new(
                redis_pool,
                ag_ui_config.max_events_per_run,
                ag_ui_config.session_ttl_sec,
            ));

            let handler = AgUiHandler::new(
                app_wrapper_module,
                app_module,
                session_manager,
                event_store,
                ag_ui_config,
            );

            boot_ag_ui_server(handler, server_config, lock, Some(shutdown_signal)).await
        }
        _ => {
            tracing::info!("AG-UI: Using Standalone mode with in-memory session/event storage");
            let session_manager =
                Arc::new(InMemorySessionManager::new(ag_ui_config.session_ttl_sec));
            let event_store = Arc::new(InMemoryEventStore::new(
                ag_ui_config.max_events_per_run,
                ag_ui_config.session_ttl_sec,
            ));

            let handler = AgUiHandler::new(
                app_wrapper_module,
                app_module,
                session_manager,
                event_store,
                ag_ui_config,
            );

            boot_ag_ui_server(handler, server_config, lock, Some(shutdown_signal)).await
        }
    }
}
