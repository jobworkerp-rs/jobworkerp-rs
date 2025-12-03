use anyhow::Result;
use command_utils::protobuf::ProtobufDescriptor;
use jobworkerp::data::RunnerData;
use prost_reflect::MessageDescriptor;

pub mod jobworkerp {
    pub mod function {
        pub mod data {
            tonic::include_proto!("jobworkerp.function.data");
        }
    }
    pub mod data {
        tonic::include_proto!("jobworkerp.data");
    }
}

// for test runner
tonic::include_proto!("_");

/// Default method name for single-method runners
///
/// All single-method runners (COMMAND, HTTP_REQUEST, PYTHON_COMMAND, etc.) use this
/// as the sole method name in their method_proto_map. Multi-method runners (MCP, Plugin)
/// may include this as one of their methods for backward compatibility.
///
/// Used as:
/// - Default fallback when `using` parameter is None in job execution
/// - HashMap key for method_descriptors lookups
/// - method_proto_map initialization for built-in runners
pub const DEFAULT_METHOD_NAME: &str = "run";

pub trait ProtobufHelper {
    fn parse_job_args_schema_descriptor(
        runner_data: &RunnerData,
        method_name: &str,
    ) -> Result<Option<MessageDescriptor>> {
        let method_proto_map = runner_data
            .method_proto_map
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("method_proto_map is required"))?;

        let method_schema = method_proto_map.schemas.get(method_name).ok_or_else(|| {
            anyhow::anyhow!(
                "Method '{}' not found in method_proto_map (available methods: {:?})",
                method_name,
                method_proto_map.schemas.keys().collect::<Vec<_>>()
            )
        })?;

        if method_schema.args_proto.is_empty() {
            return Ok(None);
        }

        let descriptor = ProtobufDescriptor::new(&method_schema.args_proto).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse args_proto for method '{}': {}",
                method_name,
                e
            )
        })?;

        let message_descriptor = descriptor.get_messages().first().cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "No message found in args_proto for method '{}': proto definition may be invalid",
                method_name
            )
        })?;

        Ok(Some(message_descriptor))
    }
    fn parse_runner_settings_schema_descriptor(
        runner_data: &RunnerData,
    ) -> Result<Option<MessageDescriptor>> {
        if runner_data.runner_settings_proto.is_empty() {
            Ok(None)
        } else {
            let descriptor = ProtobufDescriptor::new(&runner_data.runner_settings_proto)?;
            descriptor
                .get_messages()
                .first()
                .map(|m| Some(m.clone()))
                .ok_or_else(|| anyhow::anyhow!("message not found"))
        }
    }
    fn parse_job_result_schema_descriptor(
        runner_data: &RunnerData,
        method_name: &str,
    ) -> Result<Option<MessageDescriptor>> {
        let method_proto_map = runner_data
            .method_proto_map
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("method_proto_map is required"))?;

        let method_schema = method_proto_map.schemas.get(method_name).ok_or_else(|| {
            anyhow::anyhow!(
                "Method '{}' not found in method_proto_map (available methods: {:?})",
                method_name,
                method_proto_map.schemas.keys().collect::<Vec<_>>()
            )
        })?;

        if method_schema.result_proto.is_empty() {
            return Ok(None);
        }

        let descriptor = ProtobufDescriptor::new(&method_schema.result_proto).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse result_proto for method '{}': {}",
                method_name,
                e
            )
        })?;

        let message_descriptor = descriptor.get_messages().first().cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "No message found in result_proto for method '{}': proto definition may be invalid",
                method_name
            )
        })?;

        Ok(Some(message_descriptor))
    }
}

#[cfg(test)]
mod tests;
