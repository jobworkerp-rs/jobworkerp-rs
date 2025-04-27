use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

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
