//! V2 plugin whose constructor always panics.
//!
//! Used by the host's plugin loader tests to verify that a panic during
//! the `register_plugin_v2!`-generated `load_multi_method_plugin_v2`
//! does not unwind across the extern "C" boundary and abort the host.
//! The host should observe a sentinel `PluginInstanceRaw` (state = null,
//! vtable_size = 0) and return a load-time error.

use jobworkerp_plugin_abi::register_plugin_v2;
use jobworkerp_plugin_abi::v2::{CancelToken, HighLevelSink, PluginV2};
use std::collections::HashMap;

pub struct PanicCtorPlugin;

impl PanicCtorPlugin {
    // Deliberately panics — see crate docs. A Default impl would just
    // re-panic, so we suppress the clippy lint instead of adding one.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        panic!("simulated plugin constructor failure");
    }
}

#[async_trait::async_trait]
impl PluginV2 for PanicCtorPlugin {
    fn name(&self) -> String {
        "PanicCtor".to_string()
    }
    fn description(&self) -> String {
        String::new()
    }
    fn settings_schema(&self) -> String {
        String::new()
    }
    fn method_proto_map(&self) -> HashMap<String, Vec<u8>> {
        HashMap::new()
    }

    fn set_cancellation_token(&mut self, _token: CancelToken) {}

    async fn load(&mut self, _settings: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    async fn run(
        &mut self,
        _args: Vec<u8>,
        metadata: HashMap<String, String>,
        _using: Option<String>,
    ) -> (Result<Vec<u8>, String>, HashMap<String, String>) {
        (Ok(Vec::new()), metadata)
    }

    async fn run_stream(
        &mut self,
        _args: Vec<u8>,
        metadata: HashMap<String, String>,
        _using: Option<String>,
        _output: HighLevelSink,
    ) -> Result<HashMap<String, String>, String> {
        Ok(metadata)
    }
}

register_plugin_v2!(PanicCtorPlugin, PanicCtorPlugin::new());
