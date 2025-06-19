use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod definition;
pub mod execute;
pub mod runner;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfig {
    pub task_default_timeout_sec: Option<u64>,
    pub http_user_agent: String,
    pub http_timeout_sec: Duration,
    pub checkpoint_expire_sec: Option<u64>,
    pub checkpoint_max_count: Option<u32>,
}
const DEFAULT_USER_AGENT: &str = "simple-workflow/1.0";
const DEFAULT_HTTP_REQUEST_TIMEOUT_SEC: u32 = 120; // 2 minutes

impl WorkflowConfig {
    pub fn new(
        task_default_timeout_sec: Option<u64>, // XXX not in use yet
        http_user_agent: Option<String>,
        http_timeout_sec: Option<u32>,
        checkpoint_expire_sec: Option<u64>,
        checkpoint_max_count: Option<u32>,
    ) -> Self {
        Self {
            task_default_timeout_sec,
            http_user_agent: http_user_agent.unwrap_or_else(|| DEFAULT_USER_AGENT.to_string()),
            http_timeout_sec: http_timeout_sec
                .map(|sec| Duration::from_secs(sec as u64))
                .unwrap_or_else(|| Duration::from_secs(DEFAULT_HTTP_REQUEST_TIMEOUT_SEC as u64)),
            checkpoint_expire_sec,
            checkpoint_max_count,
        }
    }
    pub fn new_by_envy() -> Self {
        envy::prefixed("WORKFLOW_")
            .from_env()
            .unwrap_or_else(|_| Self {
                task_default_timeout_sec: None,
                http_user_agent: DEFAULT_USER_AGENT.to_string(),
                http_timeout_sec: Duration::from_secs(DEFAULT_HTTP_REQUEST_TIMEOUT_SEC as u64),
                checkpoint_expire_sec: None,
                checkpoint_max_count: None,
            })
    }
}
impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            task_default_timeout_sec: None,
            http_user_agent: DEFAULT_USER_AGENT.to_string(),
            http_timeout_sec: Duration::from_secs(DEFAULT_HTTP_REQUEST_TIMEOUT_SEC as u64),
            checkpoint_expire_sec: None,
            checkpoint_max_count: None,
        }
    }
}
