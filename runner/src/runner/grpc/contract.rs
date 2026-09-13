//! Descriptor-backed contract resolution shared by gRPC integrations.

use super::common::GrpcConnection;
use super::proto_source::{ProtoSourceRef, fetch_proto_source};
use crate::jobworkerp::runner::grpc::{GrpcArgs, GrpcRunnerSettings};
use anyhow::{Result, anyhow};
use command_utils::protobuf::{ProtobufDescriptor, ProtobufDescriptorLoader};
use prost_reflect::MessageDescriptor;
use std::time::Duration;

/// RPC execution mode supported by the gRPC runner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrpcMethodKind {
    Unary,
    ServerStreaming,
}

/// The currently resolved contract for a fixed gRPC worker.
#[derive(Clone, Debug)]
pub struct ResolvedGrpcContract {
    pub method: String,
    pub kind: GrpcMethodKind,
    pub input: MessageDescriptor,
    pub output: MessageDescriptor,
}

/// Resolve a fixed worker's method and its input/output descriptors.
///
/// This is deliberately a short-lived operation. Callers that need fresh
/// remote contracts create it for each request instead of retaining it as a
/// schema cache.
pub async fn resolve_fixed_grpc_contract(
    settings: &GrpcRunnerSettings,
    timeout: Duration,
) -> Result<ResolvedGrpcContract> {
    resolve_contract_inner(settings, &GrpcArgs::default(), timeout, true).await
}

/// Resolve the contract for a generic gRPC invocation. The caller-provided
/// method and optional proto source are used only when the worker settings do
/// not already fix them, matching the runner's execution precedence.
pub async fn resolve_grpc_contract(
    settings: &GrpcRunnerSettings,
    args: &GrpcArgs,
    timeout: Duration,
) -> Result<ResolvedGrpcContract> {
    resolve_contract_inner(settings, args, timeout, false).await
}

async fn resolve_contract_inner(
    settings: &GrpcRunnerSettings,
    args: &GrpcArgs,
    timeout: Duration,
    fixed: bool,
) -> Result<ResolvedGrpcContract> {
    let resolve = async {
        let method = resolve_effective_method(settings, args, fixed)?;
        if let Some(proto_source) = static_proto_source(settings, args, fixed) {
            let client = reqwest::Client::new();
            let proto = fetch_proto_source(&client, proto_source).await?;
            let pool = ProtobufDescriptor::build_protobuf_descriptor(&proto)?;
            // Static contracts are resolved entirely from their descriptor
            // pool, so the MCP process does not need an RPC connection.
            let descriptor = GrpcConnection::get_method_from_descriptor_pool(&pool, &method)?;
            return resolved_contract(method, descriptor);
        }

        let mut connection = GrpcConnection::new();
        connection.create(settings).await?;
        let descriptor_pool = connection
            .resolve_descriptor_pool(if fixed { &None } else { &args.proto })
            .await?;
        let descriptor = connection
            .get_method_descriptor(&method, descriptor_pool.as_ref())
            .await?;
        resolved_contract(method, descriptor)
    };
    tokio::time::timeout(timeout, resolve)
        .await
        .map_err(|_| anyhow!("gRPC schema resolution timed out"))?
}

fn static_proto_source<'a>(
    settings: &'a GrpcRunnerSettings,
    args: &'a GrpcArgs,
    fixed: bool,
) -> Option<ProtoSourceRef<'a>> {
    settings
        .proto
        .as_ref()
        .map(ProtoSourceRef::from_settings)
        .or_else(|| {
            (!fixed)
                .then(|| args.proto.as_ref().map(ProtoSourceRef::from_args))
                .flatten()
        })
}

fn resolve_effective_method(
    settings: &GrpcRunnerSettings,
    args: &GrpcArgs,
    fixed: bool,
) -> Result<String> {
    if let Some(method) = settings.method.as_deref()
        && !method.trim().is_empty()
    {
        return Ok(method.to_string());
    }
    if !fixed
        && let Some(method) = args.method.as_deref()
        && !method.trim().is_empty()
    {
        return Ok(method.to_string());
    }
    Err(anyhow!(
        "No gRPC method specified: set method in GrpcRunnerSettings or GrpcArgs"
    ))
}

fn resolved_contract(
    method: String,
    descriptor: prost_reflect::MethodDescriptor,
) -> Result<ResolvedGrpcContract> {
    let kind = match (
        descriptor.is_client_streaming(),
        descriptor.is_server_streaming(),
    ) {
        (false, false) => GrpcMethodKind::Unary,
        (false, true) => GrpcMethodKind::ServerStreaming,
        _ => return Err(anyhow!("client or bidirectional streaming is unsupported")),
    };
    Ok(ResolvedGrpcContract {
        method,
        kind,
        input: descriptor.input(),
        output: descriptor.output(),
    })
}

#[cfg(test)]
mod tests {
    use super::resolve_fixed_grpc_contract;
    use crate::jobworkerp::runner::grpc::{GrpcRunnerSettings, GrpcSettingsProtoSource};
    use std::time::Duration;

    #[tokio::test]
    async fn static_proto_contract_resolution_does_not_connect_to_the_rpc_server() {
        let settings = GrpcRunnerSettings {
            host: "127.0.0.1".to_string(),
            port: 1,
            method: Some("example.Echo/Call".to_string()),
            proto: Some(GrpcSettingsProtoSource {
                source: "syntax = \"proto3\"; package example; service Echo { rpc Call (Request) returns (Response); } message Request {} message Response {}".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let contract = resolve_fixed_grpc_contract(&settings, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(contract.method, "example.Echo/Call");
    }

    #[tokio::test]
    async fn static_proto_contract_reports_a_missing_method_without_connecting() {
        let settings = GrpcRunnerSettings {
            host: "127.0.0.1".to_string(),
            port: 1,
            method: Some("example.Echo/Missing".to_string()),
            proto: Some(GrpcSettingsProtoSource {
                source: "syntax = \"proto3\"; package example; service Echo { rpc Call (Request) returns (Response); } message Request {} message Response {}".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let error = resolve_fixed_grpc_contract(&settings, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Method Missing not found in service example.Echo")
        );
    }
}
