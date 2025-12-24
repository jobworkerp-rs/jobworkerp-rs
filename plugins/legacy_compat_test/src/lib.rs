use anyhow::Result;
use async_trait::async_trait;
use jobworkerp_runner::runner::plugins::PluginRunner;
use prost::Message;
use proto::jobworkerp::data::StreamingOutputType;
use std::{alloc::System, collections::HashMap};
use tracing::Level;

pub mod legacy {
    tonic::include_proto!("legacy");
}

use legacy::{LegacyArgs, LegacyResult, LegacyRunnerSettings};

#[global_allocator]
static ALLOCATOR: System = System;

// Legacy plugin FFI symbols (origin/main compatible)
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn load_plugin() -> Box<dyn PluginRunner + Send + Sync> {
    Box::new(LegacyCompatPlugin::new())
}

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_plugin(ptr: Box<dyn PluginRunner + Send + Sync>) {
    drop(ptr);
}

/// Legacy compatibility test plugin
///
/// This plugin implements the origin/main version of PluginRunner trait
/// to test backward compatibility with existing compiled plugin binaries.
pub struct LegacyCompatPlugin {
    settings: Option<LegacyRunnerSettings>,
}

impl Default for LegacyCompatPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LegacyCompatPlugin {
    pub fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .try_init();
        LegacyCompatPlugin { settings: None }
    }

    fn execute(&self, args: &[u8]) -> Result<Vec<u8>> {
        let input = LegacyArgs::decode(args).unwrap_or(LegacyArgs {
            test_input: String::from_utf8_lossy(args).to_string(),
            test_number: 0,
        });

        tracing::info!(
            "LegacyCompatPlugin executing with input: {}, number: {}",
            input.test_input,
            input.test_number
        );

        let result = LegacyResult {
            result_message: format!(
                "Legacy plugin processed: {} (number: {})",
                input.test_input, input.test_number
            ),
            success: true,
        };

        Ok(result.encode_to_vec())
    }
}

// Note: LegacyArgs, LegacyResult, and LegacyRunnerSettings already implement
// JsonSchema via build.rs configuration (line 12: type_attribute with schemars::JsonSchema).
// No need for separate schema structs - Proto types can be used directly.

// Implement origin/main PluginRunner trait
#[async_trait]
impl PluginRunner for LegacyCompatPlugin {
    fn name(&self) -> String {
        "LegacyCompat".to_string()
    }

    fn description(&self) -> String {
        "Legacy compatibility test plugin (origin/main trait)".to_string()
    }

    fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        tracing::info!("LegacyCompatPlugin load called");
        if !settings.is_empty() {
            self.settings = Some(LegacyRunnerSettings::decode(settings.as_slice())?);
        }
        Ok(())
    }

    fn run(
        &mut self,
        args: Vec<u8>,
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        (self.execute(&args), metadata)
    }

    fn begin_stream(&mut self, _arg: Vec<u8>, _metadata: HashMap<String, String>) -> Result<()> {
        Err(anyhow::anyhow!(
            "Streaming not supported in legacy compatibility plugin"
        ))
    }

    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        Err(anyhow::anyhow!(
            "Streaming not supported in legacy compatibility plugin"
        ))
    }

    fn cancel(&self) -> bool {
        tracing::warn!("LegacyCompatPlugin: cancel not implemented");
        false
    }

    fn is_canceled(&self) -> bool {
        false
    }

    fn runner_settings_proto(&self) -> String {
        include_str!("../protobuf/legacy_runner.proto").to_string()
    }

    // Origin/main methods (MUST be implemented for backward compatibility)
    fn job_args_proto(&self) -> String {
        include_str!("../protobuf/legacy_args.proto").to_string()
    }

    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../protobuf/legacy_result.proto").to_string())
    }

    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::NonStreaming
    }

    fn settings_schema(&self) -> String {
        let schema = schemars::schema_for!(LegacyRunnerSettings);
        serde_json::to_string(&schema).unwrap_or_else(|e| {
            tracing::error!("Failed to serialize settings schema: {:?}", e);
            "{}".to_string()
        })
    }

    fn arguments_schema(&self) -> String {
        let schema = schemars::schema_for!(LegacyArgs);
        serde_json::to_string(&schema).unwrap_or_else(|e| {
            tracing::error!("Failed to serialize arguments schema: {:?}", e);
            "{}".to_string()
        })
    }

    fn output_json_schema(&self) -> Option<String> {
        let schema = schemars::schema_for!(LegacyResult);
        Some(serde_json::to_string(&schema).unwrap_or_else(|e| {
            tracing::error!("Failed to serialize output schema: {:?}", e);
            "{}".to_string()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_legacy_plugin_initialization() {
        let plugin = LegacyCompatPlugin::new();
        assert_eq!(plugin.name(), "LegacyCompat");
        assert!(plugin.description().contains("origin/main"));
    }

    #[test]
    fn test_legacy_plugin_schemas() {
        let plugin = LegacyCompatPlugin::new();

        // Verify origin/main methods exist
        assert!(!plugin.job_args_proto().is_empty());
        assert!(plugin.result_output_proto().is_some());
        assert!(!plugin.arguments_schema().is_empty());
        assert!(plugin.output_json_schema().is_some());
    }

    #[test]
    fn test_legacy_plugin_execution() {
        let mut plugin = LegacyCompatPlugin::new();

        let args = LegacyArgs {
            test_input: "test".to_string(),
            test_number: 42,
        };

        let (result, _) = plugin.run(args.encode_to_vec(), HashMap::new());
        assert!(result.is_ok());

        let output = result.unwrap();
        let decoded = LegacyResult::decode(output.as_slice()).unwrap();
        assert!(decoded.success);
        assert!(decoded.result_message.contains("test"));
        assert!(decoded.result_message.contains("42"));
    }
}
