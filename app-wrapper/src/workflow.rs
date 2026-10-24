use std::time::Duration;

use serde::Deserialize;

pub mod create_workflow;
pub mod definition;
pub mod execute;
pub mod runner;

/// Loaded from `WORKFLOW_*` environment variables. Every field is optional so
/// that any subset of the variables can be set.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkflowConfig {
    pub task_default_timeout_sec: Option<u64>,
    pub checkpoint_expire_sec: Option<u64>,
    pub checkpoint_max_count: Option<u32>,
}

impl WorkflowConfig {
    pub fn new_by_envy() -> Self {
        envy::prefixed("WORKFLOW_").from_env().unwrap_or_else(|e| {
            tracing::warn!("Invalid WORKFLOW_* environment variable, using defaults: {e}");
            Self::default()
        })
    }

    /// Opt-in default timeout for run/call tasks. Unset or 0 means no timeout,
    /// matching the Open Workflow Specification where task timeouts are optional.
    pub fn task_default_timeout(&self) -> Option<Duration> {
        self.task_default_timeout_sec
            .filter(|sec| *sec > 0)
            .map(Duration::from_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_vars(vars: &[(&str, &str)]) -> Result<WorkflowConfig, envy::Error> {
        envy::prefixed("WORKFLOW_")
            .from_iter(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    #[test]
    fn env_vars_are_applied() {
        let config = from_vars(&[
            ("WORKFLOW_TASK_DEFAULT_TIMEOUT_SEC", "5"),
            ("WORKFLOW_CHECKPOINT_EXPIRE_SEC", "86400"),
            ("WORKFLOW_CHECKPOINT_MAX_COUNT", "100"),
        ])
        .unwrap();
        assert_eq!(config.task_default_timeout(), Some(Duration::from_secs(5)));
        assert_eq!(config.checkpoint_expire_sec, Some(86400));
        assert_eq!(config.checkpoint_max_count, Some(100));
    }

    #[test]
    fn unset_env_vars_stay_unset() {
        let config = from_vars(&[("WORKFLOW_TASK_DEFAULT_TIMEOUT_SEC", "5")]).unwrap();
        assert_eq!(config.task_default_timeout(), Some(Duration::from_secs(5)));
        assert_eq!(config.checkpoint_expire_sec, None);
        assert_eq!(config.checkpoint_max_count, None);
    }

    #[test]
    fn task_default_timeout_treats_unset_and_zero_as_none() {
        let config = |sec| WorkflowConfig {
            task_default_timeout_sec: sec,
            ..Default::default()
        };
        assert_eq!(config(None).task_default_timeout(), None);
        assert_eq!(config(Some(0)).task_default_timeout(), None);
        assert_eq!(
            config(Some(5)).task_default_timeout(),
            Some(Duration::from_secs(5))
        );
    }
}
