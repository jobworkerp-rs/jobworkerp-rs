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

/// Default method name for single-method runners in Phase 6.6.4+ architecture
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
    // Phase 6.6.4: job_args_proto deleted, use method_proto_map instead
    fn parse_job_args_schema_descriptor(
        runner_data: &RunnerData,
        method_name: &str,
    ) -> Result<Option<MessageDescriptor>> {
        runner_data
            .method_proto_map
            .as_ref()
            .and_then(|map| map.schemas.get(method_name))
            .and_then(|schema| {
                if schema.args_proto.is_empty() {
                    None
                } else {
                    Some(ProtobufDescriptor::new(&schema.args_proto))
                }
            })
            .transpose()?
            .and_then(|descriptor| descriptor.get_messages().first().cloned())
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("message not found"))
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
    // Phase 6.6.4: result_output_proto deleted, use MethodSchema.result_proto instead
    fn parse_job_result_schema_descriptor(
        runner_data: &RunnerData,
        method_name: &str,
    ) -> Result<Option<MessageDescriptor>> {
        runner_data
            .method_proto_map
            .as_ref()
            .and_then(|map| map.schemas.get(method_name))
            .and_then(|schema| {
                if schema.result_proto.is_empty() {
                    None
                } else {
                    Some(ProtobufDescriptor::new(&schema.result_proto))
                }
            })
            .transpose()?
            .and_then(|descriptor| descriptor.get_messages().first().cloned())
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("message not found"))
    }
}
