use crate::jobworkerp::runner::function_set_selector::FunctionSetSelectorSettings;
use crate::schema_to_json_string;
use proto::DEFAULT_METHOD_NAME;
use proto::jobworkerp::data::{RunnerType, StreamingOutputType};
use std::collections::HashMap;

use super::RunnerSpec;

#[derive(Debug, Clone)]
pub struct FunctionSetSelectorSpecImpl;

impl FunctionSetSelectorSpecImpl {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FunctionSetSelectorSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerSpec for FunctionSetSelectorSpecImpl {
    fn name(&self) -> String {
        RunnerType::FunctionSetSelector.as_str_name().to_string()
    }

    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/function_set_selector_settings.proto")
            .to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/function_set_selector_args.proto"
                )
                .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/function_set_selector_result.proto"
                )
                .to_string(),
                description: Some(
                    "List available FunctionSets (tool collections) to find the right tools for the task."
                        .to_string(),
                ),
                output_type: StreamingOutputType::NonStreaming as i32,
                ..Default::default()
            },
        );
        schemas
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(FunctionSetSelectorSettings, "settings_schema")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runner_spec_name() {
        let spec = FunctionSetSelectorSpecImpl::new();
        assert_eq!(spec.name(), "FUNCTION_SET_SELECTOR");
    }

    #[test]
    fn test_method_proto_map() {
        let spec = FunctionSetSelectorSpecImpl::new();
        let methods = spec.method_proto_map();
        assert_eq!(methods.len(), 1);
        assert!(methods.contains_key(DEFAULT_METHOD_NAME));

        let schema = &methods[DEFAULT_METHOD_NAME];
        assert!(schema.args_proto.contains("FunctionSetSelectorArgs"));
        assert!(schema.result_proto.contains("FunctionSetSelectorResult"));
        assert_eq!(schema.output_type, StreamingOutputType::NonStreaming as i32);
    }

    #[test]
    fn test_settings_schema() {
        let spec = FunctionSetSelectorSpecImpl::new();
        let schema = spec.settings_schema();
        assert!(!schema.is_empty());
        let parsed: serde_json::Value = serde_json::from_str(&schema).unwrap();
        assert!(parsed.is_object());
    }
}
