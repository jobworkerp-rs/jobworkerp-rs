use super::config::{McpConfig, McpServerConfig, McpServerTransportConfig};
use crate::runner::timeout_config::RunnerTimeoutConfig;
use anyhow::Result;
use debug_stub_derive::DebugStub;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::{MokaCache, MokaCacheConfig, MokaCacheImpl, UseMokaCache};
use rmcp::{
    model::{CallToolRequestParam, CallToolResult, LoggingLevel, Tool},
    service::{QuitReason, RunningService},
    transport::child_process::ConfigureCommandExt,
    ClientHandler, RoleClient, ServiceExt,
};
use std::{borrow::Cow, collections::HashMap, process::Stdio, sync::Arc, time::Duration};
use tokio::sync::RwLock;

#[derive(DebugStub)]
pub struct McpServerProxy {
    pub name: String,
    pub description: Option<String>,
    pub transport: RunningService<RoleClient, ()>,
    #[debug_stub = "MokaCache<Arc<String>, Vec<String>>"]
    async_cache: MokaCacheImpl<Arc<String>, Vec<Tool>>,
}
impl McpServerProxy {
    // TODO env?
    const DURATION: Duration = Duration::from_secs(3 * 60);
    const MEMORY_CACHE_CONFIG: MokaCacheConfig = MokaCacheConfig {
        ttl: Some(Self::DURATION),
        num_counters: 10000,
    };

    fn find_all_list_cache_key() -> Arc<String> {
        Arc::new("list_tools:all".to_string())
    }

    pub async fn new(config: &McpServerConfig) -> Result<Self> {
        let transport_config = config.transport.clone();
        let timeout_config = RunnerTimeoutConfig::global();

        let transport = tokio::time::timeout(
            timeout_config.mcp_connection,
            Self::start(&transport_config),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "MCP connection timeout after {:?} for '{}'",
                timeout_config.mcp_connection,
                config.name
            )
        })??;

        Ok(Self {
            name: config.name.clone(),
            description: config.description.clone(),
            transport,
            async_cache: MokaCacheImpl::new(&Self::MEMORY_CACHE_CONFIG),
        })
    }
    // start connection to mcp server
    async fn start(config: &McpServerTransportConfig) -> Result<RunningService<RoleClient, ()>> {
        let timeout_config = RunnerTimeoutConfig::global();

        let client = tokio::time::timeout(timeout_config.mcp_transport_start, async {
            let result: Result<RunningService<RoleClient, ()>> = match config {
                McpServerTransportConfig::Sse { url, headers } => {
                    let mut header_map = reqwest::header::HeaderMap::new();
                    for (key, value) in headers {
                        let header_name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                            .map_err(|e| {
                            anyhow::anyhow!("Invalid header name '{}': {}", key, e)
                        })?;
                        let header_value =
                            reqwest::header::HeaderValue::from_str(value).map_err(|e| {
                                anyhow::anyhow!("Invalid header value for '{}': {}", key, e)
                            })?;
                        header_map.insert(header_name, header_value);
                    }

                    let reqwest_client = reqwest::Client::builder()
                        .default_headers(header_map)
                        .build()
                        .map_err(|e| anyhow::anyhow!("Failed to create reqwest client: {}", e))?;

                    let transport =
                        rmcp::transport::sse_client::SseClientTransport::start_with_client(
                            reqwest_client,
                            rmcp::transport::sse_client::SseClientConfig {
                                sse_endpoint: url.as_str().into(),
                                ..Default::default()
                            },
                        )
                        .await
                        .map_err(|e| anyhow::anyhow!("SSE transport error: {}", e))?;
                    // TODO use handler
                    ().serve(transport)
                        .await
                        .map_err(|e| anyhow::anyhow!("Serve error: {}", e))
                }
                McpServerTransportConfig::Stdio {
                    command,
                    args,
                    envs,
                } => {
                    let transport = rmcp::transport::child_process::TokioChildProcess::new(
                        tokio::process::Command::new(command).configure(|cmd| {
                            cmd.args(args).envs(envs).stderr(Stdio::inherit());
                        }),
                    )
                    .map_err(|e| anyhow::anyhow!("Child process error: {}", e))?;
                    // TODO use handler
                    ().serve(transport)
                        .await
                        .map_err(|e| anyhow::anyhow!("Serve error: {}", e))
                }
            };
            result
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Transport start timeout after {:?}",
                timeout_config.mcp_transport_start
            )
        })??;

        Ok(client)
    }

    // NOTE: No timeout applied here - Application Layer (infra/runner/rows.rs:29-45) already has timeout
    pub async fn load_tools(&self) -> Result<Vec<Tool>> {
        tracing::debug!("loading mcp tools from: {}", &self.name);
        let k = Arc::new(Self::find_all_list_cache_key());
        let tools = self
            .with_cache(&k, || async {
                self.transport.peer().list_all_tools().await.map_err(|e| {
                    let mes = format!("Failed to load tools: {e}");
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
            QuitReason::JoinError(join_error) => {
                tracing::error!("tokio thread Join error: {:?}", join_error);
                Err(JobWorkerError::RuntimeError(format!("Join error: {join_error}")).into())
            }
        }
    }
}
// TODO use for serve
impl ClientHandler for McpServerProxy {
    #[allow(clippy::manual_async_fn)]
    fn on_resource_updated(
        &self,
        params: rmcp::model::ResourceUpdatedNotificationParam,
        _context: rmcp::service::NotificationContext<RoleClient>,
    ) -> impl std::prelude::rust_2024::Future<Output = ()> + Send + '_ {
        async move {
            let uri = params.uri;
            tracing::info!("Resource updated: {}", uri);
            let _ = self.async_cache.clear().await;
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn on_tool_list_changed(
        &self,
        _context: rmcp::service::NotificationContext<RoleClient>,
    ) -> impl std::prelude::rust_2024::Future<Output = ()> + Send + '_ {
        async {
            let _ = self.async_cache.clear().await;
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn on_logging_message(
        &self,
        params: rmcp::model::LoggingMessageNotificationParam,
        _context: rmcp::service::NotificationContext<RoleClient>,
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

    #[allow(clippy::manual_async_fn)]
    fn on_progress(
        &self,
        params: rmcp::model::ProgressNotificationParam,
        _context: rmcp::service::NotificationContext<RoleClient>,
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
    mcp_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
}
impl Default for McpServerFactory {
    fn default() -> Self {
        Self {
            mcp_configs: Arc::new(RwLock::new(HashMap::new())),
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
            mcp_configs: Arc::new(RwLock::new(mcp_configs)),
        }
    }
    // overwrite config if exists
    pub async fn add_server(&self, config: McpServerConfig) -> Result<McpServerProxy> {
        let server = McpServerProxy::new(&config).await?;
        let mut mcp_configs = self.mcp_configs.write().await;
        mcp_configs.insert(config.name.clone(), config.clone());
        // test connection
        Ok(server)
    }
    pub async fn remove_server(&self, name: &str) -> Result<bool> {
        // TODO shutdown?(runner pool?)
        let mut mcp_configs = self.mcp_configs.write().await;
        if mcp_configs.remove(name).is_some() {
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub async fn find_server_config(&self, name: &str) -> Option<McpServerConfig> {
        let mcp_configs = self.mcp_configs.read().await;
        mcp_configs.get(name).cloned()
    }
    pub async fn get_server_proxy(&self, name: &str) -> Result<McpServerProxy> {
        let mcp_configs = self.mcp_configs.read().await;
        let config = mcp_configs
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("MCP client not found: {}", name))?;
        let server = McpServerProxy::new(config).await?;
        Ok(server)
    }
    pub async fn find_all(&self) -> Vec<McpServerConfig> {
        self.mcp_configs.read().await.values().cloned().collect()
    }
    // boot up and connection test for all mcp servers
    pub async fn test_all(&self) -> Result<Vec<McpServerProxy>> {
        let mut mcp_clients = Vec::new();
        for (_, client) in self.mcp_configs.read().await.iter() {
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
    pub async fn connect_server(&self, name: &str) -> Result<McpServerProxy> {
        let conf = self
            .mcp_configs
            .read()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP client not found: {}", name))?;
        let server = McpServerProxy::new(&conf).await?;
        Ok(server)
    }
}

impl UseMokaCache<Arc<String>, Vec<Tool>> for McpServerProxy {
    fn cache(&self) -> &MokaCache<Arc<String>, Vec<Tool>> {
        self.async_cache.cache()
    }
}
