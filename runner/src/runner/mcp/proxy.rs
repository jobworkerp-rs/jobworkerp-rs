use anyhow::Result;
use debug_stub_derive::DebugStub;
use infra_utils::infra::{
    lock::RwLockWithKey,
    memory::{self, MemoryCacheConfig, UseMemoryCache},
};
use rmcp::{
    model::{CallToolRequestParam, CallToolResult, LoggingLevel, Tool},
    service::{QuitReason, RunningService},
    ClientHandler, Peer, RoleClient, ServiceExt,
};
use std::{borrow::Cow, collections::HashMap, process::Stdio, sync::Arc, time::Duration};
use stretto::AsyncCache;

use super::config::{McpConfig, McpServerConfig, McpServerTransportConfig};

#[derive(DebugStub)]
pub struct McpServerProxy {
    pub name: String,
    pub description: Option<String>,
    pub transport: RunningService<RoleClient, ()>,
    #[debug_stub = "AsyncCache<Arc<String>, Vec<String>>"]
    async_cache: AsyncCache<Arc<String>, Vec<Tool>>,
    key_lock: Arc<RwLockWithKey<Arc<String>>>,
}
impl McpServerProxy {
    // TODO env?
    const DURATION: Duration = Duration::from_secs(3 * 60);
    const MEMORY_CACHE_CONFIG: MemoryCacheConfig = MemoryCacheConfig {
        num_counters: 10000,
        max_cost: 10000,
        use_metrics: false,
    };

    fn find_all_list_cache_key() -> Arc<String> {
        Arc::new("list_tools:all".to_string())
    }

    pub async fn new(config: &McpServerConfig) -> Result<Self> {
        let transport_config = config.transport.clone();
        Ok(Self {
            name: config.name.clone(),
            description: config.description.clone(),
            transport: Self::start(&transport_config).await?,
            async_cache: memory::new_memory_cache(&Self::MEMORY_CACHE_CONFIG),
            key_lock: Arc::new(RwLockWithKey::new(
                Self::MEMORY_CACHE_CONFIG.max_cost as usize,
            )),
        })
    }
    // start connection to mcp server
    async fn start(config: &McpServerTransportConfig) -> Result<RunningService<RoleClient, ()>> {
        let client = match config {
            McpServerTransportConfig::Sse { url } => {
                let transport = rmcp::transport::sse::SseTransport::start(url).await?;
                // TODO use handler
                ().serve(transport).await?
            }
            McpServerTransportConfig::Stdio {
                command,
                args,
                envs,
            } => {
                let transport = rmcp::transport::child_process::TokioChildProcess::new(
                    tokio::process::Command::new(command)
                        .args(args)
                        .envs(envs)
                        .stderr(Stdio::inherit())
                        .stdout(Stdio::inherit()),
                )?;
                // TODO use handler
                ().serve(transport).await?
            }
        };
        Ok(client)
    }

    pub async fn load_tools(&self) -> Result<Vec<Tool>> {
        tracing::debug!("loading mcp tools from: {}", &self.name);
        let k = Arc::new(Self::find_all_list_cache_key());
        let tools = self
            .with_cache_locked(&k, None, || async {
                self.transport.peer().list_all_tools().await.map_err(|e| {
                    let mes = format!("Failed to load tools: {}", e);
                    tracing::error!(mes);
                    anyhow::anyhow!(mes)
                })
            })
            .await?;

        Ok(tools)
    }
    pub async fn call_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<CallToolResult> {
        let arguments = match args {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        };

        let call_result = self
            .transport
            .call_tool(CallToolRequestParam {
                name: Cow::Owned(tool_name.to_string()),
                arguments,
            })
            .await?;
        Ok(call_result)
    }
    pub async fn cancel(self) -> Result<bool> {
        match self.transport.cancel().await? {
            QuitReason::Cancelled => Ok(true),
            QuitReason::Closed => Ok(false),
        }
    }
}
// TODO use for serve
impl ClientHandler for McpServerProxy {
    async fn on_resource_updated(&self, params: rmcp::model::ResourceUpdatedNotificationParam) {
        let uri = params.uri;
        tracing::info!("Resource updated: {}", uri);
        let _ = self
            .async_cache
            .clear()
            .await
            .inspect_err(|e| tracing::error!("Failed to clear cache: {}", e));
    }

    fn set_peer(&mut self, peer: Peer<RoleClient>) {
        tracing::warn!("McpServerProxy: set_peer not supported");
        drop(peer)
    }

    fn get_peer(&self) -> Option<Peer<RoleClient>> {
        Some(self.transport.peer().clone())
    }

    fn on_tool_list_changed(
        &self,
    ) -> impl std::prelude::rust_2024::Future<Output = ()> + Send + '_ {
        async {
            let _ = self.async_cache.clear().await.inspect_err(|e| {
                tracing::error!("on_tool_list_changed: Failed to clear cache: {}", e)
            });
        }
    }
    fn on_logging_message(
        &self,
        params: rmcp::model::LoggingMessageNotificationParam,
    ) -> impl std::prelude::rust_2024::Future<Output = ()> + Send + '_ {
        async move {
            match params.level {
                LoggingLevel::Emergency
                | LoggingLevel::Alert
                | LoggingLevel::Critical
                | LoggingLevel::Error => {
                    tracing::error!(
                        "logger={:?}, Logging message={}",
                        params.logger,
                        params.data
                    );
                }
                LoggingLevel::Warning => {
                    tracing::warn!(
                        "logger={:?}, Logging message={}",
                        params.logger,
                        params.data
                    );
                }
                LoggingLevel::Notice | LoggingLevel::Info => {
                    tracing::info!(
                        "logger={:?}, Logging message={}",
                        params.logger,
                        params.data
                    );
                }
                LoggingLevel::Debug => {
                    tracing::debug!(
                        "logger={:?}, Logging message={}",
                        params.logger,
                        params.data
                    );
                }
            }
        }
    }
    fn on_progress(
        &self,
        params: rmcp::model::ProgressNotificationParam,
    ) -> impl std::prelude::rust_2024::Future<Output = ()> + Send + '_ {
        async move {
            tracing::info!(
                "Progress: {}/{} ({})",
                params.progress,
                params.total.unwrap_or_default(),
                params.message.unwrap_or_default()
            );
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpServerFactory {
    mcp_configs: Arc<HashMap<String, McpServerConfig>>,
}
impl Default for McpServerFactory {
    fn default() -> Self {
        Self {
            mcp_configs: Arc::new(HashMap::new()),
        }
    }
}
impl McpServerFactory {
    // create all mcp clients from config
    pub fn new(config: McpConfig) -> Self {
        let mut mcp_configs = HashMap::new();

        for server in config.server {
            mcp_configs.insert(server.name.clone(), server);
        }

        Self {
            mcp_configs: Arc::new(mcp_configs),
        }
    }
    pub fn find_all(&self) -> Vec<McpServerConfig> {
        self.mcp_configs
            .iter()
            .map(|(_, client)| client.clone())
            .collect()
    }
    // boot up and connection test for all mcp servers
    pub async fn test_all(&self) -> Result<Vec<McpServerProxy>> {
        let mut mcp_clients = Vec::new();
        for (_, client) in self.mcp_configs.iter() {
            match McpServerProxy::new(client).await {
                Ok(s) => {
                    tracing::info!("MCP server {} can be connected", client.name);
                    mcp_clients.push(s)
                }
                Err(e) => {
                    tracing::error!("Failed to connect to {}: {:#?}", client.name, e);
                    return Err(e);
                }
            }
        }
        Ok(mcp_clients)
    }
    pub async fn create_server(&self, name: &str) -> Result<McpServerProxy> {
        let conf = self
            .mcp_configs
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP client not found: {}", name))?;
        let server = McpServerProxy::new(&conf).await?;
        Ok(server)
    }
}

impl UseMemoryCache<Arc<String>, Vec<Tool>> for McpServerProxy {
    fn cache(&self) -> &AsyncCache<Arc<String>, Vec<Tool>> {
        &self.async_cache
    }

    #[doc = " default cache ttl"]
    fn default_ttl(&self) -> Option<&Duration> {
        Some(&Self::DURATION)
    }

    fn key_lock(&self) -> &RwLockWithKey<Arc<String>> {
        &self.key_lock
    }
}
