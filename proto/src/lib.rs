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

/// Extension trait for RetryPolicy to calculate total timeout with retries
pub trait RetryPolicyExt {
    /// Calculate total wait timeout including all retries and retry intervals
    ///
    /// This calculates the maximum time needed to wait for a job to complete,
    /// including all retry attempts and the intervals between them.
    ///
    /// # Arguments
    /// * `job_timeout_ms` - Single job execution timeout in milliseconds
    ///
    /// # Returns
    /// Total timeout in milliseconds: (job_timeout × (max_retry + 1)) + sum of retry intervals
    fn calculate_total_timeout_ms(&self, job_timeout_ms: u64) -> u64;
}

impl RetryPolicyExt for jobworkerp::data::RetryPolicy {
    fn calculate_total_timeout_ms(&self, job_timeout_ms: u64) -> u64 {
        use jobworkerp::data::RetryType;

        let retry_count = self.max_retry as u64;
        if retry_count == 0 {
            return job_timeout_ms;
        }

        // Total execution time: job_timeout × (retry_count + 1)
        let total_execution_time = job_timeout_ms * (retry_count + 1);

        // Calculate sum of all retry intervals
        let retry_type = RetryType::try_from(self.r#type).unwrap_or(RetryType::None);
        let total_interval: u64 = match retry_type {
            RetryType::None => 0,
            RetryType::Constant => {
                // interval × retry_count
                self.interval as u64 * retry_count
            }
            RetryType::Linear => {
                // interval×1 + interval×2 + ... + interval×n = interval × n(n+1)/2
                let sum = retry_count * (retry_count + 1) / 2;
                self.interval as u64 * sum
            }
            RetryType::Exponential => {
                // interval×basis^0 + interval×basis^1 + ... + interval×basis^(n-1)
                // = interval × (basis^n - 1) / (basis - 1)
                let mut sum: u64 = 0;
                for i in 0..retry_count {
                    let interval =
                        (self.interval as f32 * self.basis.powf(i as f32)).round() as u64;
                    // Apply max_interval cap
                    let capped_interval = interval.min(self.max_interval as u64);
                    sum += capped_interval;
                }
                sum
            }
        };

        total_execution_time + total_interval
    }
}

/// Calculate total wait timeout for Direct response jobs
///
/// Convenience function that handles optional RetryPolicy.
/// Returns None for unlimited timeout (when job_timeout_ms is 0).
///
/// # Arguments
/// * `job_timeout_ms` - Single job execution timeout in milliseconds (0 means unlimited)
/// * `retry_policy` - Optional retry policy
///
/// # Returns
/// * `Some(timeout)` - Total timeout in milliseconds including retries
/// * `None` - Unlimited timeout (when job_timeout_ms is 0)
pub fn calculate_direct_response_timeout_ms(
    job_timeout_ms: u64,
    retry_policy: Option<&jobworkerp::data::RetryPolicy>,
) -> Option<u64> {
    if job_timeout_ms == 0 {
        None
    } else {
        match retry_policy {
            Some(policy) => Some(policy.calculate_total_timeout_ms(job_timeout_ms)),
            None => Some(job_timeout_ms),
        }
    }
}

#[cfg(test)]
mod tests;
