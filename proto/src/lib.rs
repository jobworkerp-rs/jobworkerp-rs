use anyhow::Result;
use command_utils::protobuf::ProtobufDescriptor;
use jobworkerp::data::RunnerData;
use prost_reflect::MessageDescriptor;

pub mod jobworkerp {
    pub mod data {
        tonic::include_proto!("jobworkerp.data");
    }
}

// for test runner
tonic::include_proto!("_");

pub trait ProtobufHelper {
    fn parse_job_args_schema_descriptor(
        runner_data: &RunnerData,
    ) -> Result<Option<MessageDescriptor>> {
        if runner_data.job_args_proto.is_empty() {
            Ok(None)
        } else {
            let descriptor = ProtobufDescriptor::new(&runner_data.job_args_proto)?;
            descriptor
                .get_messages()
                .first()
                .map(|m| Some(m.clone()))
                .ok_or_else(|| anyhow::anyhow!("message not found"))
        }
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
}
