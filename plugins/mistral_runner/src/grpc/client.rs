//! gRPC FunctionService client for tool calling
//!
//! Provides access to tool definitions and execution via gRPC

use anyhow::Result;
use std::collections::HashMap;
use std::time::Duration;
use tokio_stream::StreamExt;
use tonic::transport::Channel;

// Include generated gRPC code from build.rs
pub mod generated {
    pub mod jobworkerp {
        pub mod data {
            tonic::include_proto!("jobworkerp.data");
        }
        pub mod function {
            pub mod data {
                tonic::include_proto!("jobworkerp.function.data");
            }
            pub mod service {
                tonic::include_proto!("jobworkerp.function.service");
            }
        }
    }
}

use generated::jobworkerp::function::data::{FunctionCallOptions, FunctionSpecs};
use generated::jobworkerp::function::service::{
    function_service_client::FunctionServiceClient as GrpcClient, FindFunctionSetRequest,
    FunctionCallRequest,
};

/// gRPC client for FunctionService
pub struct FunctionServiceClient {
    client: GrpcClient<Channel>,
}

impl FunctionServiceClient {
    /// Connect to the FunctionService
    pub async fn connect(endpoint: &str, connection_timeout_sec: u32) -> Result<Self> {
        let channel = Channel::from_shared(endpoint.to_string())?
            .connect_timeout(Duration::from_secs(connection_timeout_sec as u64))
            .timeout(Duration::from_secs(300))
            .connect()
            .await?;

        Ok(Self {
            client: GrpcClient::new(channel),
        })
    }

    /// Get function specs by function set name
    pub async fn find_functions_by_set(&self, set_name: &str) -> Result<Vec<FunctionSpecs>> {
        let request = FindFunctionSetRequest {
            name: set_name.to_string(),
        };

        let response = self.client.clone().find_list_by_set(request).await?;

        let mut stream = response.into_inner();
        let mut functions = Vec::new();

        while let Some(spec) = stream.next().await {
            match spec {
                Ok(s) => functions.push(s),
                Err(e) => {
                    tracing::warn!("Error receiving function spec: {:?}", e);
                }
            }
        }

        tracing::debug!("Found {} functions in set '{}'", functions.len(), set_name);
        Ok(functions)
    }

    /// Call a function by name
    pub async fn call_function(
        &self,
        function_name: &str,
        args_json: &str,
        timeout_sec: u32,
        metadata: HashMap<String, String>,
    ) -> Result<String> {
        use generated::jobworkerp::function::service::function_call_request::Name;

        let request = FunctionCallRequest {
            name: Some(Name::RunnerName(function_name.to_string())),
            runner_parameters: None,
            args_json: args_json.to_string(),
            uniq_key: None,
            options: Some(FunctionCallOptions {
                timeout_ms: Some((timeout_sec as i64) * 1000),
                streaming: Some(false),
                metadata,
            }),
        };

        let response = self.client.clone().call(request).await?;

        let mut stream = response.into_inner();
        let mut output = String::new();
        let mut error_message: Option<String> = None;

        while let Some(result) = stream.next().await {
            match result {
                Ok(r) => {
                    if !r.output.is_empty() {
                        output.push_str(&r.output);
                    }
                    if let Some(err) = r.error_message {
                        error_message = Some(err);
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Function call stream error: {:?}", e));
                }
            }
        }

        if let Some(error) = error_message {
            if !error.is_empty() {
                return Err(anyhow::anyhow!("Function call error: {}", error));
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_connect() {
        let client = FunctionServiceClient::connect("http://localhost:9000", 10).await;
        assert!(client.is_ok());
    }
}
