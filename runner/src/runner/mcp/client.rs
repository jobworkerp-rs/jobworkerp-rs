use anyhow::Result;
use rmcp::{
    model::{CallToolRequestParam, CallToolResult, Tool},
    service::{QuitReason, RunningService},
    RoleClient, ServiceExt,
};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::HashMap, path::Path, process::Stdio, sync::Arc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub server: Vec<McpServerConfig>,
}

impl McpConfig {
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = tokio::fs::read_to_string(path).await?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub description: Option<String>,
    #[serde(flatten)]
    pub transport: McpServerTransportConfig,
}

pub trait UseMcpServerConfigs {
    fn mcp_configs(&self) -> &HashMap<String, McpServerConfig>;
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "protocol", rename_all = "lowercase")]
pub enum McpServerTransportConfig {
    Sse {
        url: String,
    },
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        envs: HashMap<String, String>,
    },
}

#[derive(Debug)]
pub struct McpServer {
    pub name: String,
    pub description: Option<String>,
    pub transport: RunningService<RoleClient, ()>,
}
impl McpServer {
    pub async fn new(config: &McpServerConfig) -> Result<Self> {
        let transport_config = config.transport.clone();
        Ok(Self {
            name: config.name.clone(),
            description: config.description.clone(),
            transport: Self::start(&transport_config).await?,
        })
    }
    // start connection to mcp server
    async fn start(config: &McpServerTransportConfig) -> Result<RunningService<RoleClient, ()>> {
        let client = match config {
            McpServerTransportConfig::Sse { url } => {
                let transport = rmcp::transport::sse::SseTransport::start(url).await?;
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
                ().serve(transport).await?
            }
        };
        Ok(client)
    }

    pub async fn load_tools(&self) -> Result<Vec<Tool>> {
        tracing::debug!("loading mcp tools from: {}", &self.name);
        let mut res = Vec::new();
        let server = self.transport.peer().clone();
        let tools = server.list_all_tools().await?;

        for tool in tools {
            tracing::debug!("adding tool: {}", &tool.name);
            res.push(tool);
        }
        Ok(res)
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
    pub async fn test_all(&self) -> Result<Vec<McpServer>> {
        let mut mcp_clients = Vec::new();
        for (_, client) in self.mcp_configs.iter() {
            match McpServer::new(client).await {
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
    pub async fn create_server(&self, name: &str) -> Result<McpServer> {
        let conf = self
            .mcp_configs
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP client not found: {}", name))?;
        let server = McpServer::new(&conf).await?;
        Ok(server)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_mcp_config_load() -> Result<()> {
        // create temp file for test
        let mut temp_file = NamedTempFile::new()?;
        let config_content = r#"
            [[server]]
            name = "test-server"
            description = "test server"
            protocol = "sse"
            url = "http://localhost:8080"

            [[server]]
            name = "test-stdio"
            protocol = "stdio"
            command = "echo"
            args = ["hello", "world"]
            envs = { TEST_ENV = "test_value" }
        "#;

        write!(temp_file, "{}", config_content)?;
        temp_file.flush()?;

        // load
        let config = McpConfig::load(temp_file.path()).await?;
        assert_eq!(config.server.len(), 2);

        let first_server = &config.server[0];
        assert_eq!(first_server.name, "test-server");
        assert_eq!(first_server.description, Some("test server".to_string()));
        match &first_server.transport {
            McpServerTransportConfig::Sse { url } => {
                assert_eq!(url, "http://localhost:8080");
            }
            _ => panic!("Expected Sse transport"),
        }

        let second_server = &config.server[1];
        assert_eq!(second_server.name, "test-stdio");
        assert_eq!(second_server.description, None);
        match &second_server.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                envs,
            } => {
                assert_eq!(command, "echo");
                assert_eq!(args, &["hello", "world"]);
                assert_eq!(envs.get("TEST_ENV"), Some(&"test_value".to_string()));
            }
            _ => panic!("Expected Stdio transport"),
        }

        Ok(())
    }
}
