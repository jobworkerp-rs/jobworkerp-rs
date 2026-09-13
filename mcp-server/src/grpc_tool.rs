//! MCP-only projection of fixed gRPC workers.

use crate::description::{self, ParsedDescription};
use crate::proto_schema::{message_to_protojson_schema, oneof_exclusion_constraints};
use anyhow::Result;
use app::app::function::{EnqueuedFunction, FunctionApp, FunctionAppImpl};
use app::app::worker::UseWorkerApp;
use base64::Engine;
use command_utils::protobuf::ProtobufDescriptor;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::grpc::{
    GrpcArgs, GrpcRunnerSettings, GrpcStreamingResult, GrpcUnaryResult, grpc_streaming_result,
    grpc_unary_result,
};
use jobworkerp_runner::runner::grpc::contract::{
    GrpcMethodKind, resolve_fixed_grpc_contract, resolve_grpc_contract,
};
use proto::jobworkerp::data::result_output_item;
use proto::jobworkerp::data::{ResponseType, RunnerType};
use proto::jobworkerp::function::data::FunctionSpecs;
use rmcp::model::{CallToolResult, Tool};
use std::sync::Arc;
use std::time::Duration;

/// The descriptor-independent result wrapper for direct/generic GRPC tools.
/// Fixed workers use the stricter descriptor-derived schema below.
pub fn generic_grpc_output_schema() -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({
        "type":"object",
        "properties":{
            "output":{},
            "executionInfo": execution_info_schema(),
            "error":{"type":"object","properties":{
                "stage":{"enum":["input","execution","response_decode"]},
                "message":{"type":"string","minLength":1}
            },"required":["stage","message"],"additionalProperties":false}
        },
        "additionalProperties":false,
        "oneOf":[
            {"required":["output"],"not":{"required":["error"]}},
            {"required":["error"]}
        ]
    })
    .as_object()
    .expect("JSON object")
    .clone()
}

/// Resolve and run a direct GRPC runner through the normal enqueue path.
/// `None` means the arguments are not the runner `{settings, arguments}` form.
pub async fn call_generic_grpc_tool(
    function_app: &FunctionAppImpl,
    tool_name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
    request_meta: Option<serde_json::Value>,
    timeout: Duration,
    timeout_sec: u32,
) -> Result<Option<CallToolResult>> {
    if has_runner_meta(request_meta.as_ref()) {
        return Ok(Some(stage_error(
            "input",
            anyhow::anyhow!("_meta.jobworkerp/runner is supported only by fixed gRPC worker tools"),
        )));
    }
    let (Some(settings_value), Some(args_value)) =
        (arguments.get("settings"), arguments.get("arguments"))
    else {
        return Ok(None);
    };
    let settings = match decode_grpc_settings(settings_value) {
        Ok(settings) => settings,
        Err(error) => return Ok(Some(stage_error("input", error))),
    };
    let args = match decode_grpc_args(args_value) {
        Ok(args) => args,
        Err(error) => return Ok(Some(stage_error("input", error))),
    };
    let contract = match resolve_grpc_contract(&settings, &args, timeout).await {
        Ok(contract) => contract,
        Err(_) => {
            let (collected, ()) =
                match enqueue_and_collect(function_app, tool_name, arguments, timeout_sec, |_| {
                    Ok(())
                })
                .await
                {
                    Ok(result) => result,
                    Err(error) => return Ok(Some(stage_error("execution", error))),
                };
            return decode_generic_result(
                collected.bytes.as_slice(),
                tool_name,
                collected.execution_info,
            )
            .map(Some)
            .or_else(|error| Ok(Some(stage_error("response_decode", error))));
        }
    };
    let (collected, envelope_kind) =
        match enqueue_and_collect(function_app, tool_name, arguments, timeout_sec, |using| {
            runner_result_kind(using, tool_name)
        })
        .await
        {
            Ok(result) => result,
            Err(error) => return Ok(Some(stage_error("execution", error))),
        };
    match decode_result(
        collected.bytes.as_slice(),
        &contract.output,
        envelope_kind,
        collected.execution_info,
    ) {
        Ok(result) => Ok(Some(result)),
        Err(error) => Ok(Some(stage_error("response_decode", error))),
    }
}

fn decode_grpc_settings(value: &serde_json::Value) -> Result<GrpcRunnerSettings> {
    decode_grpc_proto_json(
        include_str!("../../runner/protobuf/jobworkerp/runner/grpc/runner.proto"),
        "jobworkerp.runner.grpc.GrpcRunnerSettings",
        value,
    )
}

fn decode_grpc_args(value: &serde_json::Value) -> Result<GrpcArgs> {
    decode_grpc_proto_json(
        include_str!("../../runner/protobuf/jobworkerp/runner/grpc/args.proto"),
        "jobworkerp.runner.grpc.GrpcArgs",
        value,
    )
}

fn decode_grpc_proto_json<T>(
    proto: &str,
    message_name: &str,
    value: &serde_json::Value,
) -> Result<T>
where
    T: prost::Message + Default,
{
    let source = proto.to_string();
    let descriptor = ProtobufDescriptor::new(&source)?
        .get_message_by_name(message_name)
        .ok_or_else(|| anyhow::anyhow!("gRPC control message descriptor is missing"))?;
    let bytes = ProtobufDescriptor::json_value_to_message(descriptor, value, false, false)?;
    ProstMessageCodec::deserialize_message(&bytes)
}

/// Execute through FunctionApp's existing enqueue path and project the raw
/// runner output. This module deliberately does not create a gRPC client.
pub async fn call_fixed_grpc_tool(
    function_app: &FunctionAppImpl,
    tool_name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
    request_meta: Option<serde_json::Value>,
    timeout: Duration,
    timeout_sec: u32,
) -> Result<Option<CallToolResult>> {
    let Some((worker_name, using)) = tool_name.rsplit_once("___") else {
        return Ok(None);
    };
    let Some(worker) = function_app
        .worker_app()
        .find_data_by_name(worker_name)
        .await?
    else {
        return Ok(None);
    };
    let settings =
        ProstMessageCodec::deserialize_message::<GrpcRunnerSettings>(&worker.runner_settings);
    let Ok(settings) = settings else {
        return Ok(None);
    };
    if settings
        .method
        .as_deref()
        .is_none_or(|method| method.trim().is_empty())
    {
        return Ok(None);
    }
    let contract = match resolve_fixed_grpc_contract(&settings, timeout).await {
        Ok(contract) => contract,
        Err(error) => {
            tracing::warn!(worker = %worker.name, error = %error, "fixed gRPC tool is no longer publishable");
            return Ok(Some(not_found_result()));
        }
    };
    let expected = match contract.kind {
        GrpcMethodKind::Unary => jobworkerp_runner::runner::grpc::METHOD_UNARY,
        GrpcMethodKind::ServerStreaming => jobworkerp_runner::runner::grpc::METHOD_STREAMING,
    };
    if using != expected {
        return Ok(Some(not_found_result()));
    }
    let arguments = arguments;
    if let Err(error) = reject_duplicate_field_aliases(&contract.input, &arguments) {
        return Ok(Some(stage_error("input", error)));
    }
    let bytes = match ProtobufDescriptor::json_value_to_message(
        contract.input.clone(),
        &serde_json::Value::Object(arguments),
        false,
        false,
    ) {
        Ok(bytes) => bytes,
        Err(error) => return Ok(Some(stage_error("input", error))),
    };
    let mut runner_arguments = serde_json::Map::from_iter([(
        "body".to_string(),
        serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
    )]);
    if let Err(error) = apply_invocation_meta(&mut runner_arguments, request_meta) {
        return Ok(Some(stage_error("input", error)));
    }
    let (collected, envelope_kind) = match enqueue_and_collect(
        function_app,
        tool_name,
        runner_arguments,
        timeout_sec,
        |using| runner_result_kind(using, tool_name),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return Ok(Some(stage_error("execution", error))),
    };
    match decode_result(
        collected.bytes.as_slice(),
        &contract.output,
        envelope_kind,
        collected.execution_info,
    ) {
        Ok(result) => Ok(Some(result)),
        Err(error) => Ok(Some(stage_error("response_decode", error))),
    }
}

fn reject_duplicate_field_aliases(
    input: &prost_reflect::MessageDescriptor,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    for field in input.fields() {
        let canonical = field.json_name();
        let proto = field.name();
        if canonical != proto && arguments.contains_key(canonical) && arguments.contains_key(proto)
        {
            return Err(anyhow::anyhow!(
                "gRPC input field {canonical} was specified by both its JSON and proto names"
            ));
        }
    }
    Ok(())
}

fn apply_invocation_meta(
    runner_arguments: &mut serde_json::Map<String, serde_json::Value>,
    meta: Option<serde_json::Value>,
) -> Result<()> {
    let Some(meta) = meta else {
        return Ok(());
    };
    let meta = meta
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("_meta must be an object"))?;
    let Some(value) = meta.get("jobworkerp/runner") else {
        return Ok(());
    };
    let value = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("_meta.jobworkerp/runner must be an object"))?;
    for key in value.keys() {
        if key != "timeout" && key != "metadata" {
            return Err(anyhow::anyhow!(
                "_meta.jobworkerp/runner contains an unsupported key: {key}"
            ));
        }
    }
    if let Some(timeout) = value.get("timeout") {
        let timeout = timeout
            .as_u64()
            .filter(|value| *value <= u32::MAX as u64)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "_meta.jobworkerp/runner.timeout must be an unsigned 32-bit integer"
                )
            })?;
        runner_arguments.insert(
            "timeout".to_string(),
            serde_json::Value::Number(timeout.into()),
        );
    }
    if let Some(metadata) = value.get("metadata") {
        let metadata = metadata
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("_meta.jobworkerp/runner.metadata must be an object"))?;
        let mut converted = serde_json::Map::new();
        for (key, value) in metadata {
            let value = value.as_str().ok_or_else(|| {
                anyhow::anyhow!("_meta.jobworkerp/runner.metadata values must be strings")
            })?;
            validate_rpc_metadata(key, value)?;
            converted.insert(key.clone(), serde_json::Value::String(value.to_string()));
        }
        runner_arguments.insert("metadata".to_string(), serde_json::Value::Object(converted));
    }
    Ok(())
}

fn validate_rpc_metadata(key: &str, value: &str) -> Result<()> {
    if key.ends_with("-bin") {
        tonic::metadata::MetadataKey::<tonic::metadata::Binary>::from_bytes(key.as_bytes())
            .map_err(|_| anyhow::anyhow!("invalid binary gRPC metadata key: {key}"))?;
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(|_| anyhow::anyhow!("binary gRPC metadata value must be standard Base64"))?;
    } else {
        tonic::metadata::MetadataKey::<tonic::metadata::Ascii>::from_bytes(key.as_bytes())
            .map_err(|_| anyhow::anyhow!("invalid gRPC metadata key: {key}"))?;
        tonic::metadata::MetadataValue::<tonic::metadata::Ascii>::try_from(value)
            .map_err(|_| anyhow::anyhow!("invalid gRPC metadata value for key: {key}"))?;
    }
    Ok(())
}

fn has_runner_meta(meta: Option<&serde_json::Value>) -> bool {
    meta.and_then(serde_json::Value::as_object)
        .is_some_and(|meta| meta.contains_key("jobworkerp/runner"))
}

/// The selected runner method determines the protobuf envelope stored in the
/// job result. The reflected RPC method determines only the body descriptor.
fn runner_result_kind(using: Option<&str>, tool_name: &str) -> Result<GrpcMethodKind> {
    let method = using
        .or_else(|| tool_name.rsplit_once("___").map(|(_, using)| using))
        .ok_or_else(|| anyhow::anyhow!("gRPC tool name has no runner method"))?;
    match method {
        jobworkerp_runner::runner::grpc::METHOD_UNARY => Ok(GrpcMethodKind::Unary),
        jobworkerp_runner::runner::grpc::METHOD_STREAMING => Ok(GrpcMethodKind::ServerStreaming),
        _ => Err(anyhow::anyhow!("unsupported gRPC runner method: {method}")),
    }
}

#[derive(Debug)]
struct CollectedRawResult {
    bytes: Vec<u8>,
    execution_info: serde_json::Value,
}

/// Enqueue through the existing FunctionApp path and collect its raw result.
/// `prepare` runs after enqueue but before waiting for completion, preserving
/// callers' existing validation order for the selected runner method.
async fn enqueue_and_collect<T>(
    function_app: &FunctionAppImpl,
    tool_name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
    timeout_sec: u32,
    prepare: impl FnOnce(Option<&str>) -> Result<T>,
) -> Result<(CollectedRawResult, T)> {
    let enqueued = function_app
        .enqueue_function_for_llm(
            Arc::new(Default::default()),
            tool_name,
            Some(arguments),
            timeout_sec,
        )
        .await?;
    let prepared = prepare(enqueued.using.as_deref())?;
    let collected = collect_raw_result(enqueued).await?;
    Ok((collected, prepared))
}

async fn collect_raw_result(enqueued: EnqueuedFunction) -> Result<CollectedRawResult> {
    let job_id = enqueued.job_id.value.to_string();
    if let Some(result) = enqueued.raw_result {
        return Ok(CollectedRawResult {
            bytes: output_bytes(&result)?,
            execution_info: execution_info(&result, job_id),
        });
    }
    let handle = enqueued
        .result_handle
        .ok_or_else(|| anyhow::anyhow!("streaming job has no result listener"))?;
    let (result, stream) = handle
        .await
        .map_err(|error| anyhow::anyhow!("result listener failed: {error}"))??;
    ensure_successful_job_result(&result)?;
    if let Some(mut stream) = stream {
        use futures::StreamExt;
        while let Some(item) = stream.next().await {
            if let Some(result_output_item::Item::FinalCollected(bytes)) = item.item {
                return Ok(CollectedRawResult {
                    bytes,
                    execution_info: execution_info(&result, job_id),
                });
            }
        }
    }
    Ok(CollectedRawResult {
        bytes: output_bytes(&result)?,
        execution_info: execution_info(&result, job_id),
    })
}

fn execution_info(
    result: &proto::jobworkerp::data::JobResult,
    job_id: String,
) -> serde_json::Value {
    let mut info =
        serde_json::Map::from_iter([("jobId".to_string(), serde_json::Value::String(job_id))]);
    if let Some(data) = &result.data {
        if data.start_time != 0 {
            info.insert(
                "startedAt".to_string(),
                serde_json::Value::String(data.start_time.to_string()),
            );
        }
        if data.end_time != 0 {
            info.insert(
                "completedAt".to_string(),
                serde_json::Value::String(data.end_time.to_string()),
            );
        }
        if data.start_time != 0
            && let Some(duration) = data.end_time.checked_sub(data.start_time)
            && duration >= 0
        {
            info.insert(
                "executionTimeMs".to_string(),
                serde_json::Value::String(duration.to_string()),
            );
        }
    }
    if !result.metadata.is_empty() {
        info.insert(
            "metadata".to_string(),
            serde_json::to_value(&result.metadata).expect("string metadata must serialize"),
        );
    }
    serde_json::Value::Object(info)
}

fn output_bytes(result: &proto::jobworkerp::data::JobResult) -> Result<Vec<u8>> {
    let data = ensure_successful_job_result(result)?;
    data.output
        .as_ref()
        .filter(|output| !output.items.is_empty())
        .map(|output| output.items.clone())
        .ok_or_else(|| anyhow::anyhow!("job completed without runner output"))
}

fn ensure_successful_job_result(
    result: &proto::jobworkerp::data::JobResult,
) -> Result<&proto::jobworkerp::data::JobResultData> {
    let data = result
        .data
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("job completed without result data"))?;
    if data.status() != proto::jobworkerp::data::ResultStatus::Success {
        let diagnostic = data
            .output
            .as_ref()
            .filter(|output| !output.items.is_empty())
            .map(|output| String::from_utf8_lossy(&output.items).into_owned())
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| format!("job completed with status {:?}", data.status()));
        return Err(anyhow::anyhow!("job execution failed: {diagnostic}"));
    }
    Ok(data)
}

fn decode_result(
    bytes: &[u8],
    output: &prost_reflect::MessageDescriptor,
    kind: GrpcMethodKind,
    execution_info: serde_json::Value,
) -> Result<CallToolResult> {
    let value = match kind {
        GrpcMethodKind::Unary => {
            let result = ProstMessageCodec::deserialize_message::<GrpcUnaryResult>(bytes)?;
            decode_unary_result(result, output)?
        }
        GrpcMethodKind::ServerStreaming => {
            let result = ProstMessageCodec::deserialize_message::<GrpcStreamingResult>(bytes)?;
            decode_streaming_result(result, output)?
        }
    };
    Ok(finalize_grpc_result(value, execution_info))
}

/// Add the MCP execution envelope after a runner result has been decoded.
/// Body projection intentionally remains with the descriptor-aware and
/// descriptor-independent decoding paths because their error payloads differ.
fn finalize_grpc_result(
    mut value: serde_json::Value,
    execution_info: serde_json::Value,
) -> CallToolResult {
    let is_error = value
        .get("output")
        .and_then(|output| output.get("code"))
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|code| code != 0);
    value
        .as_object_mut()
        .expect("gRPC MCP result must be an object")
        .insert("executionInfo".to_string(), execution_info);
    if is_error {
        let message = value
            .pointer("/output/message")
            .and_then(serde_json::Value::as_str)
            .filter(|message| !message.trim().is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| "gRPC call returned a non-OK status".to_string());
        value
            .as_object_mut()
            .expect("gRPC MCP result must be an object")
            .insert(
                "error".to_string(),
                serde_json::json!({"stage":"execution", "message":message}),
            );
        CallToolResult::structured_error(value)
    } else {
        CallToolResult::structured(value)
    }
}

/// Decode the runner envelope without relying on an RPC response descriptor.
/// JSON payloads were already converted by the runner and remain usable;
/// protobuf payloads are exposed as Base64 because their message schema is
/// unavailable to the MCP server.
fn decode_generic_result(
    bytes: &[u8],
    tool_name: &str,
    execution_info: serde_json::Value,
) -> Result<CallToolResult> {
    let using = tool_name
        .rsplit_once("___")
        .map(|(_, using)| using)
        .ok_or_else(|| anyhow::anyhow!("gRPC tool name has no runner method"))?;
    match using {
        jobworkerp_runner::runner::grpc::METHOD_UNARY => {
            let result = ProstMessageCodec::deserialize_message::<GrpcUnaryResult>(bytes)?;
            generic_unary_result(result, execution_info)
        }
        jobworkerp_runner::runner::grpc::METHOD_STREAMING => {
            let result = ProstMessageCodec::deserialize_message::<GrpcStreamingResult>(bytes)?;
            generic_streaming_result(result, execution_info)
        }
        _ => Err(anyhow::anyhow!("unsupported gRPC runner method: {using}")),
    }
}

fn generic_unary_result(
    result: GrpcUnaryResult,
    execution_info: serde_json::Value,
) -> Result<CallToolResult> {
    let (body_encoding, body) = match result.response_data {
        Some(grpc_unary_result::ResponseData::Body(body)) => (
            "protobuf_base64",
            serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(body)),
        ),
        Some(grpc_unary_result::ResponseData::JsonBody(body)) => {
            ("json", serde_json::from_str(&body)?)
        }
        None => ("protobuf_base64", serde_json::Value::Null),
    };
    generic_result(
        result.code,
        result.metadata,
        result.message,
        "body",
        body,
        body_encoding,
        execution_info,
    )
}

fn generic_streaming_result(
    result: GrpcStreamingResult,
    execution_info: serde_json::Value,
) -> Result<CallToolResult> {
    let (body_encoding, bodies) = match result.response_data {
        Some(grpc_streaming_result::ResponseData::Bodies(bodies)) => (
            "protobuf_base64",
            serde_json::Value::Array(
                bodies
                    .items
                    .into_iter()
                    .map(|body| {
                        serde_json::Value::String(
                            base64::engine::general_purpose::STANDARD.encode(body),
                        )
                    })
                    .collect(),
            ),
        ),
        Some(grpc_streaming_result::ResponseData::JsonBody(bodies)) => {
            ("json", serde_json::from_str(&bodies)?)
        }
        None => ("protobuf_base64", serde_json::Value::Array(Vec::new())),
    };
    generic_result(
        result.code,
        result.metadata,
        result.message,
        "bodies",
        bodies,
        body_encoding,
        execution_info,
    )
}

fn generic_result(
    code: i32,
    metadata: std::collections::HashMap<String, String>,
    message: Option<String>,
    body_key: &str,
    body: serde_json::Value,
    body_encoding: &str,
    execution_info: serde_json::Value,
) -> Result<CallToolResult> {
    let output = serde_json::json!({
        "code": code,
        "metadata": metadata,
        "message": message,
        "bodyEncoding": body_encoding,
        body_key: body,
    });
    Ok(finalize_grpc_result(
        serde_json::json!({"output": output}),
        execution_info,
    ))
}

fn decode_unary_result(
    result: GrpcUnaryResult,
    output: &prost_reflect::MessageDescriptor,
) -> Result<serde_json::Value> {
    if result.code != 0 {
        return Ok(serde_json::json!({
            "output":{"code":result.code,"metadata":result.metadata,"message":result.message}
        }));
    }
    let body = match result.response_data {
        Some(grpc_unary_result::ResponseData::JsonBody(value)) => serde_json::from_str(&value)?,
        Some(grpc_unary_result::ResponseData::Body(value)) => {
            let message = ProtobufDescriptor::get_message_from_bytes(output.clone(), &value)?;
            ProtobufDescriptor::message_to_json_value(&message)?
        }
        None => serde_json::Value::Null,
    };
    Ok(
        serde_json::json!({"output":{"code":result.code,"metadata":result.metadata,"message":result.message,"bodyEncoding":"json","body":body}}),
    )
}

fn decode_streaming_result(
    result: GrpcStreamingResult,
    output: &prost_reflect::MessageDescriptor,
) -> Result<serde_json::Value> {
    if result.code != 0 {
        return Ok(serde_json::json!({
            "output":{"code":result.code,"metadata":result.metadata,"message":result.message}
        }));
    }
    let bodies = match result.response_data {
        Some(grpc_streaming_result::ResponseData::JsonBody(value)) => serde_json::from_str(&value)?,
        Some(grpc_streaming_result::ResponseData::Bodies(value)) => serde_json::Value::Array(
            value
                .items
                .into_iter()
                .map(|item| {
                    let message =
                        ProtobufDescriptor::get_message_from_bytes(output.clone(), &item)?;
                    ProtobufDescriptor::message_to_json_value(&message)
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        None => serde_json::Value::Array(Vec::new()),
    };
    Ok(
        serde_json::json!({"output":{"code":result.code,"metadata":result.metadata,"message":result.message,"bodyEncoding":"json","bodies":bodies}}),
    )
}

fn not_found_result() -> CallToolResult {
    CallToolResult::structured_error(
        serde_json::json!({"error":{"stage":"input","message":"fixed gRPC tool is not currently published"}}),
    )
}

fn stage_error(stage: &str, error: anyhow::Error) -> CallToolResult {
    CallToolResult::structured_error(
        serde_json::json!({"error":{"stage":stage,"message":error.to_string()}}),
    )
}

/// Return `None` when a function retains the legacy tool projection. `Some` is
/// returned for fixed gRPC workers; an empty list means that the worker must be
/// hidden because its current RPC contract cannot be resolved.
pub async fn fixed_grpc_tools(
    function_app: &FunctionAppImpl,
    function: &FunctionSpecs,
    timeout: Duration,
) -> Result<Option<Vec<Tool>>> {
    if RunnerType::try_from(function.runner_type).ok() != Some(RunnerType::Grpc) {
        return Ok(None);
    }
    let Some(worker_id) = function.worker_id.as_ref() else {
        return Ok(None);
    };
    let Some(worker) = function_app.worker_app().find_data(worker_id).await? else {
        return Ok(Some(Vec::new()));
    };
    let settings =
        ProstMessageCodec::deserialize_message::<GrpcRunnerSettings>(&worker.runner_settings)?;
    if settings
        .method
        .as_deref()
        .is_none_or(|method| method.trim().is_empty())
    {
        return Ok(None);
    }
    if !fixed_grpc_response_is_collectable(worker.response_type) {
        tracing::warn!(worker = %worker.name, response_type = worker.response_type, "fixed gRPC worker is excluded from MCP tools because it cannot return a result");
        return Ok(Some(Vec::new()));
    }

    let contract = match resolve_fixed_grpc_contract(&settings, timeout).await {
        Ok(contract) => contract,
        Err(error) => {
            tracing::warn!(worker = %worker.name, error = %error, "fixed gRPC worker is excluded from MCP tools");
            return Ok(Some(Vec::new()));
        }
    };
    let method = match contract.kind {
        GrpcMethodKind::Unary => jobworkerp_runner::runner::grpc::METHOD_UNARY,
        GrpcMethodKind::ServerStreaming => jobworkerp_runner::runner::grpc::METHOD_STREAMING,
    };
    if !function
        .methods
        .as_ref()
        .is_some_and(|methods| methods.schemas.contains_key(method))
    {
        tracing::warn!(worker = %worker.name, method, "fixed gRPC RPC shape does not match the worker's exposed methods");
        return Ok(Some(Vec::new()));
    }
    let input_schema = match message_to_protojson_schema(&contract.input) {
        Ok(schema) => schema,
        Err(error) => {
            tracing::warn!(worker = %worker.name, error = %error, "fixed gRPC worker has an unsafe input schema and is excluded from MCP tools");
            return Ok(Some(Vec::new()));
        }
    };
    let Some(mut input_schema) = mcp_object_input_schema(input_schema) else {
        tracing::warn!(worker = %worker.name, "fixed gRPC worker has a non-object request type and is excluded from MCP tools");
        return Ok(Some(Vec::new()));
    };
    add_proto_field_aliases(&mut input_schema, &contract.input);
    let output = match message_to_protojson_schema(&contract.output) {
        Ok(schema) => schema,
        Err(error) => {
            tracing::warn!(worker = %worker.name, error = %error, "fixed gRPC worker has an unsafe output schema and is excluded from MCP tools");
            return Ok(Some(Vec::new()));
        }
    };
    let output_schema = grpc_result_schema(output, contract.kind);
    let fallback_description = fixed_grpc_fallback_description(function, method, &worker.name);
    let description = apply_structured_description(
        &worker.description,
        &fallback_description,
        &contract.method,
        &contract.input,
        &mut input_schema,
        &worker.name,
    );
    let tool = Tool::new(
        format!("{}___{}", function.name, method),
        description,
        Arc::new(input_schema),
    )
    .with_raw_output_schema(Arc::new(output_schema));
    Ok(Some(vec![tool]))
}

fn fixed_grpc_response_is_collectable(response_type: i32) -> bool {
    response_type == ResponseType::Direct as i32
}

/// Return a description independent from the worker description. The latter
/// may be a declared structured document that failed validation and must never
/// be exposed as an MCP tool description on the fallback path.
fn fixed_grpc_fallback_description(
    function: &FunctionSpecs,
    method: &str,
    worker_name: &str,
) -> String {
    function
        .methods
        .as_ref()
        .and_then(|methods| methods.schemas.get(method))
        .and_then(|schema| schema.description.as_deref())
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("Fixed gRPC worker {worker_name} ({method})"))
}

fn apply_structured_description(
    raw: &str,
    fallback: &str,
    method: &str,
    input: &prost_reflect::MessageDescriptor,
    input_schema: &mut serde_json::Map<String, serde_json::Value>,
    worker_name: &str,
) -> String {
    match description::parse(raw) {
        ParsedDescription::Plain => non_blank_or(raw, fallback),
        ParsedDescription::InvalidDeclared(reason) => {
            tracing::warn!(
                worker = worker_name,
                reason,
                "structured worker description is invalid; using default tool description"
            );
            fallback.to_string()
        }
        ParsedDescription::Structured(description) => {
            if description
                .rpc
                .as_deref()
                .is_some_and(|rpc| normalize_method(rpc) != normalize_method(method))
            {
                tracing::warn!(worker = worker_name, rpc = ?description.rpc, method, "structured worker description RPC does not match fixed gRPC method");
                return fallback.to_string();
            }
            if let Some(properties) = input_schema
                .get_mut("properties")
                .and_then(serde_json::Value::as_object_mut)
            {
                for (field, annotation) in description.parameters {
                    let Some(field_descriptor) = input.fields().find(|candidate| {
                        candidate.json_name() == field || candidate.name() == field
                    }) else {
                        tracing::warn!(
                            worker = worker_name,
                            field,
                            "structured description names an unknown gRPC input field"
                        );
                        continue;
                    };
                    let names = PublicFieldNames::from_descriptor(&field_descriptor);
                    let Some(property) = properties
                        .get_mut(&names.canonical)
                        .and_then(serde_json::Value::as_object_mut)
                    else {
                        tracing::warn!(
                            worker = worker_name,
                            field,
                            "structured description names an unknown gRPC input field"
                        );
                        continue;
                    };
                    if !examples_are_valid(input, &names.canonical, annotation.get("examples")) {
                        tracing::warn!(
                            worker = worker_name,
                            field,
                            "structured description example is incompatible with the gRPC input field"
                        );
                        continue;
                    }
                    apply_field_annotation(property, &annotation);
                    if names.has_alias()
                        && let Some(alias) = properties
                            .get_mut(&names.proto)
                            .and_then(serde_json::Value::as_object_mut)
                    {
                        apply_field_annotation(alias, &annotation);
                    }
                }
            }
            non_blank_or(&description.description, fallback)
        }
    }
}

fn apply_field_annotation(
    property: &mut serde_json::Map<String, serde_json::Value>,
    annotation: &serde_json::Value,
) {
    if let Some(text) = annotation
        .get("description")
        .and_then(serde_json::Value::as_str)
    {
        property.insert(
            "description".to_string(),
            serde_json::Value::String(text.to_string()),
        );
    }
    if let Some(examples) = annotation.get("examples") {
        property.insert("examples".to_string(), examples.clone());
    }
}

#[derive(Clone)]
struct PublicFieldNames {
    proto: String,
    canonical: String,
}

impl PublicFieldNames {
    fn from_descriptor(field: &prost_reflect::FieldDescriptor) -> Self {
        Self {
            proto: field.name().to_string(),
            canonical: field.json_name().to_string(),
        }
    }

    fn has_alias(&self) -> bool {
        self.proto != self.canonical
    }

    fn all(&self) -> Vec<String> {
        if self.has_alias() {
            vec![self.canonical.clone(), self.proto.clone()]
        } else {
            vec![self.canonical.clone()]
        }
    }
}

fn public_field_names(input: &prost_reflect::MessageDescriptor) -> Vec<PublicFieldNames> {
    input
        .fields()
        .map(|field| PublicFieldNames::from_descriptor(&field))
        .collect()
}

fn add_proto_field_aliases(
    schema: &mut serde_json::Map<String, serde_json::Value>,
    input: &prost_reflect::MessageDescriptor,
) {
    let fields = public_field_names(input);
    let aliases = fields
        .iter()
        .filter(|field| field.has_alias())
        .cloned()
        .collect::<Vec<_>>();
    if aliases.is_empty() {
        return;
    }
    let required = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let required_aliases = aliases
        .iter()
        .filter(|field| {
            required
                .iter()
                .any(|name| name.as_str() == Some(&field.canonical))
        })
        .cloned()
        .collect::<Vec<_>>();
    if !required_aliases.is_empty()
        && let Some(required) = schema
            .get_mut("required")
            .and_then(serde_json::Value::as_array_mut)
    {
        required.retain(|name| {
            !required_aliases
                .iter()
                .any(|field| name.as_str() == Some(&field.canonical))
        });
        if required.is_empty() {
            schema.remove("required");
        }
    }
    let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let mut constraints = Vec::new();
    for field in &aliases {
        if let Some(property) = properties.get(&field.canonical).cloned() {
            properties.insert(field.proto.clone(), property);
            constraints.push(serde_json::json!({
                "not": {"required": [field.canonical, field.proto]}
            }));
        }
    }
    constraints.extend(required_aliases.into_iter().map(|field| {
        serde_json::json!({
            "anyOf": [
                {"required": [field.canonical]},
                {"required": [field.proto]}
            ]
        })
    }));
    let oneof_aliases = fields
        .into_iter()
        .map(|field| (field.proto.clone(), field.all()))
        .collect();
    constraints.extend(oneof_exclusion_constraints(input, &oneof_aliases));
    if !constraints.is_empty() {
        let all_of = schema
            .entry("allOf".to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if let Some(all_of) = all_of.as_array_mut() {
            all_of.extend(constraints);
        }
    }
}

fn mcp_object_input_schema(
    schema: serde_json::Value,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let serde_json::Value::Object(schema) = schema else {
        return None;
    };
    (schema.get("type") == Some(&serde_json::Value::String("object".to_string()))).then_some(schema)
}

fn examples_are_valid(
    input: &prost_reflect::MessageDescriptor,
    field: &str,
    examples: Option<&serde_json::Value>,
) -> bool {
    let Some(examples) = examples else {
        return true;
    };
    let Some(examples) = examples.as_array() else {
        return false;
    };
    examples.iter().all(|example| {
        ProtobufDescriptor::json_value_to_message(
            input.clone(),
            &serde_json::json!({field: example}),
            false,
            false,
        )
        .is_ok()
    })
}

fn normalize_method(method: &str) -> &str {
    method.trim().trim_start_matches('/')
}

fn non_blank_or(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn grpc_result_schema(
    mut body_schema: serde_json::Value,
    kind: GrpcMethodKind,
) -> serde_json::Map<String, serde_json::Value> {
    // A nested schema with local `$defs` needs its own resource identifier;
    // otherwise `#/$defs/...` resolves against the outer result wrapper.
    if body_schema.get("$defs").is_some() {
        body_schema["$id"] =
            serde_json::Value::String("urn:jobworkerp:mcp:protobuf-response-body".to_string());
    }
    let body_key = match kind {
        GrpcMethodKind::Unary => "body",
        GrpcMethodKind::ServerStreaming => "bodies",
    };
    let body_value = match kind {
        GrpcMethodKind::Unary => body_schema,
        GrpcMethodKind::ServerStreaming => {
            serde_json::json!({"type":"array", "items": body_schema})
        }
    };
    let mut output_properties = serde_json::Map::new();
    output_properties.insert(
        "code".to_string(),
        serde_json::json!({"type":"integer", "minimum":0, "maximum":16}),
    );
    output_properties.insert(
        "metadata".to_string(),
        serde_json::json!({"type":"object", "additionalProperties":{"type":"string"}}),
    );
    output_properties.insert(
        "message".to_string(),
        // Some MCP clients do not support JSON Schema's array form of `type`.
        // Keep the same string-or-null contract in the broadly supported form.
        serde_json::json!({"anyOf":[{"type":"string"}, {"type":"null"}]}),
    );
    output_properties.insert(
        "bodyEncoding".to_string(),
        serde_json::json!({"const":"json"}),
    );
    output_properties.insert(body_key.to_string(), body_value);
    serde_json::json!({
        "type":"object",
        "properties": {
            "output": {"type":"object", "properties":output_properties, "required":["code", "metadata"], "additionalProperties":false},
            "executionInfo": execution_info_schema(),
            "error": {"type":"object", "properties":{"stage":{"enum":["input","execution","response_decode"]},"message":{"type":"string", "minLength":1}},"required":["stage","message"],"additionalProperties":false}
        },
        "additionalProperties":false,
        "oneOf":[
            {
                "required":["output"],
                "not":{"required":["error"]},
                "properties":{"output":{"required":["code", "metadata", "bodyEncoding", body_key], "properties":{"code":{"const":0}}}}
            },
            {"required":["error"]}
        ]
    }).as_object().expect("JSON object").clone()
}

fn execution_info_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "minProperties": 1,
        "properties": {
            "jobId": {"type": "string", "minLength": 1},
            "startedAt": {"type": "string", "pattern": "^-?(0|[1-9][0-9]*)$"},
            "completedAt": {"type": "string", "pattern": "^-?(0|[1-9][0-9]*)$"},
            "executionTimeMs": {"type": "string", "pattern": "^(0|[1-9][0-9]*)$"},
            "metadata": {"type": "object", "additionalProperties": {"type": "string"}}
        },
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::{
        GrpcMethodKind, add_proto_field_aliases, apply_invocation_meta,
        apply_structured_description, collect_raw_result, decode_generic_result, decode_grpc_args,
        decode_result, execution_info, fixed_grpc_fallback_description,
        fixed_grpc_response_is_collectable, generic_grpc_output_schema, grpc_result_schema,
        has_runner_meta, mcp_object_input_schema, runner_result_kind,
    };
    use crate::proto_schema::message_to_protojson_schema;
    use app::app::function::EnqueuedFunction;
    use command_utils::protobuf::ProtobufDescriptor;
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use jobworkerp_runner::jobworkerp::runner::grpc::{
        GrpcStreamingResult, GrpcUnaryResult, StreamBodies, grpc_streaming_result,
        grpc_unary_result,
    };
    use proto::jobworkerp::data::{JobResult, JobResultData, ResultOutput, ResultStatus};
    use proto::jobworkerp::function::data::{FunctionSpecs, MethodSchema, MethodSchemaMap};

    #[test]
    fn result_schema_uses_the_matching_body_field() {
        let unary = grpc_result_schema(serde_json::json!({"type":"object"}), GrpcMethodKind::Unary);
        assert!(
            unary["properties"]["output"]["properties"]
                .get("body")
                .is_some()
        );
        let stream = grpc_result_schema(
            serde_json::json!({"type":"object"}),
            GrpcMethodKind::ServerStreaming,
        );
        assert!(
            stream["properties"]["output"]["properties"]
                .get("bodies")
                .is_some()
        );
        assert_eq!(
            unary["oneOf"][0]["properties"]["output"]["properties"]["code"]["const"],
            0
        );
    }

    #[test]
    fn result_schema_message_accepts_only_string_null_or_omission() {
        let schema = serde_json::Value::Object(grpc_result_schema(
            serde_json::json!({"type":"object"}),
            GrpcMethodKind::Unary,
        ));
        assert_eq!(
            schema["properties"]["output"]["properties"]["message"],
            serde_json::json!({"anyOf":[{"type":"string"}, {"type":"null"}]})
        );
        let validator = jsonschema::draft202012::new(&schema).unwrap();
        let output = |message: Option<serde_json::Value>| {
            let mut output = serde_json::json!({
                "code": 0,
                "metadata": {},
                "bodyEncoding": "json",
                "body": {}
            });
            if let Some(message) = message {
                output
                    .as_object_mut()
                    .expect("result output is an object")
                    .insert("message".to_string(), message);
            }
            serde_json::json!({"output": output})
        };

        assert!(validator.is_valid(&output(Some(serde_json::json!("completed")))));
        assert!(validator.is_valid(&output(Some(serde_json::Value::Null))));
        assert!(validator.is_valid(&output(None)));
        for invalid_message in [
            serde_json::json!(1),
            serde_json::json!(true),
            serde_json::json!({"detail":"unexpected"}),
        ] {
            assert!(!validator.is_valid(&output(Some(invalid_message))));
        }
    }

    #[test]
    fn only_object_schemas_are_valid_mcp_tool_inputs() {
        assert!(mcp_object_input_schema(serde_json::json!({"type":"object"})).is_some());
        assert!(mcp_object_input_schema(serde_json::json!({"type":"string"})).is_none());
        assert!(mcp_object_input_schema(serde_json::json!({"type":"array"})).is_none());
        assert!(mcp_object_input_schema(serde_json::json!({})).is_none());
    }

    #[test]
    fn fixed_grpc_tools_require_a_direct_response() {
        assert!(fixed_grpc_response_is_collectable(
            proto::jobworkerp::data::ResponseType::Direct as i32
        ));
        assert!(!fixed_grpc_response_is_collectable(
            proto::jobworkerp::data::ResponseType::NoResult as i32
        ));
    }

    #[test]
    fn fixed_grpc_fallback_uses_the_selected_method_description() {
        let function = FunctionSpecs {
            methods: Some(MethodSchemaMap {
                schemas: [(
                    "unary".to_string(),
                    MethodSchema {
                        description: Some("Invoke the configured gRPC method".to_string()),
                        ..Default::default()
                    },
                )]
                .into_iter()
                .collect(),
            }),
            description: "format: jobworkerp-description\nversion: 1".to_string(),
            ..Default::default()
        };

        assert_eq!(
            fixed_grpc_fallback_description(&function, "unary", "echo"),
            "Invoke the configured gRPC method"
        );
        assert_eq!(
            fixed_grpc_fallback_description(&function, "streaming", "echo"),
            "Fixed gRPC worker echo (streaming)"
        );
    }

    #[test]
    fn invalid_structured_description_uses_the_independent_fallback() {
        let descriptor =
            ProtobufDescriptor::new(&"syntax = \"proto3\"; message Request {}".to_string())
                .unwrap()
                .get_message_by_name("Request")
                .unwrap();
        let mut schema = serde_json::json!({"type":"object", "properties":{}})
            .as_object()
            .cloned()
            .unwrap();

        let description = apply_structured_description(
            "format: jobworkerp-description\nversion: 2\ndescription: rejected YAML\n",
            "Invoke the configured gRPC method",
            "example.Echo/Call",
            &descriptor,
            &mut schema,
            "echo",
        );

        assert_eq!(description, "Invoke the configured gRPC method");

        let mismatched_description = apply_structured_description(
            "format: jobworkerp-description\nversion: 1\ndescription: rejected YAML\nrpc: example.Echo/Other\n",
            "Invoke the configured gRPC method",
            "example.Echo/Call",
            &descriptor,
            &mut schema,
            "echo",
        );

        assert_eq!(mismatched_description, "Invoke the configured gRPC method");
    }

    #[test]
    fn required_proto_field_aliases_accept_either_name_but_not_both() {
        let descriptor = ProtobufDescriptor::new(
            &"syntax = \"proto2\"; message Request { required string user_id = 1; }".to_string(),
        )
        .unwrap()
        .get_message_by_name("Request")
        .unwrap();
        let mut schema = message_to_protojson_schema(&descriptor)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap();
        add_proto_field_aliases(&mut schema, &descriptor);
        let validator = jsonschema::draft202012::new(&serde_json::Value::Object(schema)).unwrap();

        assert!(validator.is_valid(&serde_json::json!({"userId":"123"})));
        assert!(validator.is_valid(&serde_json::json!({"user_id":"123"})));
        assert!(!validator.is_valid(&serde_json::json!({})));
        assert!(!validator.is_valid(&serde_json::json!({"userId":"123", "user_id":"123"})));
    }

    #[test]
    fn oneof_constraints_cover_proto_field_aliases() {
        let descriptor = ProtobufDescriptor::new(
            &"syntax = \"proto3\"; message Request { oneof selector { string user_id = 1; string display_name = 2; } }".to_string(),
        )
        .unwrap()
        .get_message_by_name("Request")
        .unwrap();
        let mut schema = message_to_protojson_schema(&descriptor)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap();
        add_proto_field_aliases(&mut schema, &descriptor);
        let validator = jsonschema::draft202012::new(&serde_json::Value::Object(schema)).unwrap();

        assert!(validator.is_valid(&serde_json::json!({"user_id":"123"})));
        assert!(validator.is_valid(&serde_json::json!({"displayName":"abc"})));
        assert!(!validator.is_valid(&serde_json::json!({"userId":"123", "display_name":"abc"})));
        assert!(!validator.is_valid(&serde_json::json!({"user_id":"123", "displayName":"abc"})));
    }

    #[test]
    fn structured_description_annotates_canonical_and_proto_field_names() {
        let descriptor = ProtobufDescriptor::new(
            &"syntax = \"proto3\"; message Request { string user_id = 1; }".to_string(),
        )
        .unwrap()
        .get_message_by_name("Request")
        .unwrap();
        let mut schema = message_to_protojson_schema(&descriptor)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap();
        add_proto_field_aliases(&mut schema, &descriptor);

        let description = apply_structured_description(
            "format: jobworkerp-description\nversion: 1\ndescription: send a request\nparameters:\n  user_id:\n    description: stable user identifier\n    examples:\n      - user-1\n",
            "fallback",
            "example.Echo/Call",
            &descriptor,
            &mut schema,
            "worker",
        );

        assert_eq!(description, "send a request");
        for name in ["userId", "user_id"] {
            assert_eq!(
                schema["properties"][name]["description"],
                serde_json::json!("stable user identifier")
            );
            assert_eq!(
                schema["properties"][name]["examples"],
                serde_json::json!(["user-1"])
            );
        }
    }

    #[test]
    fn recursive_response_schema_keeps_refs_local_to_the_body_schema() {
        let descriptor = ProtobufDescriptor::new(
            &"syntax = \"proto3\"; message Node { Node child = 1; }".to_string(),
        )
        .unwrap()
        .get_message_by_name("Node")
        .unwrap();
        let schema = grpc_result_schema(
            message_to_protojson_schema(&descriptor).unwrap(),
            GrpcMethodKind::Unary,
        );
        let validator = jsonschema::draft202012::new(&serde_json::Value::Object(schema)).unwrap();
        assert!(validator.is_valid(&serde_json::json!({
            "output": {"code":0, "metadata":{}, "bodyEncoding":"json", "body":{"child":{"child":{}}}}
        })));
    }

    #[test]
    fn runner_meta_rejects_unknown_keys() {
        let mut arguments = serde_json::Map::new();
        let error = apply_invocation_meta(
            &mut arguments,
            Some(serde_json::json!({"jobworkerp/runner":{"unexpected":true}})),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsupported key"));
    }

    #[test]
    fn runner_meta_rejects_invalid_grpc_metadata_before_execution() {
        let mut arguments = serde_json::Map::new();
        let error = apply_invocation_meta(
            &mut arguments,
            Some(serde_json::json!({
                "jobworkerp/runner":{"metadata":{"invalid key":"value"}}
            })),
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid gRPC metadata key"));

        let error = apply_invocation_meta(
            &mut arguments,
            Some(serde_json::json!({
                "jobworkerp/runner":{"metadata":{"trace-bin":"not base64"}}
            })),
        )
        .unwrap_err();
        assert!(error.to_string().contains("Base64"));
    }

    #[test]
    fn runner_meta_is_detected_only_in_request_metadata() {
        assert!(has_runner_meta(Some(&serde_json::json!({
            "jobworkerp/runner": {"timeout": 10}
        }))));
        assert!(!has_runner_meta(Some(&serde_json::json!({
            "unrelated": {"timeout": 10}
        }))));
    }

    #[test]
    fn execution_info_uses_canonical_string_fields() {
        let result = proto::jobworkerp::data::JobResult {
            data: Some(proto::jobworkerp::data::JobResultData {
                start_time: 12,
                end_time: 20,
                ..Default::default()
            }),
            metadata: [("trace-id".to_string(), "abc".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        assert_eq!(
            execution_info(&result, "42".to_string()),
            serde_json::json!({
                "jobId":"42",
                "startedAt":"12",
                "completedAt":"20",
                "executionTimeMs":"8",
                "metadata":{"trace-id":"abc"}
            })
        );
    }

    #[test]
    fn generic_output_schema_distinguishes_success_and_error() {
        let schema = generic_grpc_output_schema();
        assert_eq!(
            schema["oneOf"][0]["required"],
            serde_json::json!(["output"])
        );
        assert_eq!(schema["oneOf"][1]["required"], serde_json::json!(["error"]));
    }

    #[test]
    fn generic_grpc_arguments_use_protojson_field_names() {
        let args = decode_grpc_args(&serde_json::json!({
            "method":"example.Echo/Unary",
            "jsonBody":"{}",
            "asJson":false
        }))
        .unwrap();
        assert_eq!(args.method.as_deref(), Some("example.Echo/Unary"));
        assert_eq!(args.as_json, Some(false));
    }

    #[test]
    fn unknown_response_descriptor_preserves_unary_payload_as_base64() {
        let bytes = ProstMessageCodec::serialize_message(&GrpcUnaryResult {
            metadata: [("request-id".to_string(), "abc".to_string())]
                .into_iter()
                .collect(),
            code: 0,
            message: None,
            response_data: Some(grpc_unary_result::ResponseData::Body(vec![1, 2, 3])),
        })
        .unwrap();

        let result =
            decode_generic_result(&bytes, "GRPC___unary", serde_json::json!({"jobId":"1"}))
                .unwrap();
        assert!(!result.is_error.unwrap_or_default());
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/output/bodyEncoding"))
                .and_then(serde_json::Value::as_str),
            Some("protobuf_base64")
        );
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/output/body"))
                .and_then(serde_json::Value::as_str),
            Some("AQID")
        );
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/executionInfo/jobId"))
                .and_then(serde_json::Value::as_str),
            Some("1")
        );
    }

    #[test]
    fn unknown_response_descriptor_keeps_grpc_status_as_execution_error() {
        let bytes = ProstMessageCodec::serialize_message(&GrpcUnaryResult {
            metadata: Default::default(),
            code: 14,
            message: Some("unavailable".to_string()),
            response_data: None,
        })
        .unwrap();

        let result =
            decode_generic_result(&bytes, "GRPC___unary", serde_json::json!({"jobId":"1"}))
                .unwrap();
        assert!(result.is_error.unwrap_or_default());
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/error/stage"))
                .and_then(serde_json::Value::as_str),
            Some("execution")
        );
    }

    #[test]
    fn unknown_response_descriptor_preserves_unary_json_payload() {
        let bytes = ProstMessageCodec::serialize_message(&GrpcUnaryResult {
            metadata: Default::default(),
            code: 0,
            message: None,
            response_data: Some(grpc_unary_result::ResponseData::JsonBody(
                r#"{"value":"ok"}"#.to_string(),
            )),
        })
        .unwrap();

        let result =
            decode_generic_result(&bytes, "GRPC___unary", serde_json::json!({"jobId":"1"}))
                .unwrap();
        assert!(!result.is_error.unwrap_or_default());
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/output/bodyEncoding")),
            Some(&serde_json::json!("json"))
        );
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/output/body/value")),
            Some(&serde_json::json!("ok"))
        );
    }

    #[test]
    fn unknown_response_descriptor_preserves_streaming_json_payload() {
        let bytes = ProstMessageCodec::serialize_message(&GrpcStreamingResult {
            metadata: Default::default(),
            code: 0,
            message: None,
            response_data: Some(grpc_streaming_result::ResponseData::JsonBody(
                r#"[{"value":"ok"}]"#.to_string(),
            )),
        })
        .unwrap();

        let result =
            decode_generic_result(&bytes, "GRPC___streaming", serde_json::json!({"jobId":"1"}))
                .unwrap();
        assert!(!result.is_error.unwrap_or_default());
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/output/bodies/0/value")),
            Some(&serde_json::json!("ok"))
        );
    }

    #[test]
    fn result_envelope_follows_the_selected_runner_method() {
        assert_eq!(
            runner_result_kind(Some("streaming"), "GRPC___unary").unwrap(),
            GrpcMethodKind::ServerStreaming
        );
        assert_eq!(
            runner_result_kind(None, "GRPC___unary").unwrap(),
            GrpcMethodKind::Unary
        );
    }

    #[test]
    fn streaming_runner_result_is_not_decoded_as_a_unary_envelope() {
        let descriptor = ProtobufDescriptor::new(
            &"syntax = \"proto3\"; message Response { string value = 1; }".to_string(),
        )
        .unwrap()
        .get_message_by_name("Response")
        .unwrap();
        let body = ProtobufDescriptor::json_value_to_message(
            descriptor.clone(),
            &serde_json::json!({"value":"ok"}),
            false,
            false,
        )
        .unwrap();
        let bytes = ProstMessageCodec::serialize_message(&GrpcStreamingResult {
            metadata: Default::default(),
            code: 0,
            message: None,
            response_data: Some(grpc_streaming_result::ResponseData::Bodies(StreamBodies {
                items: vec![body],
            })),
        })
        .unwrap();

        let result = decode_result(
            &bytes,
            &descriptor,
            GrpcMethodKind::ServerStreaming,
            serde_json::json!({"jobId":"1"}),
        )
        .unwrap();
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/output/bodies/0/value")),
            Some(&serde_json::json!("ok"))
        );
    }

    #[test]
    fn descriptor_aware_error_keeps_its_existing_bodyless_shape() {
        let descriptor = ProtobufDescriptor::new(
            &"syntax = \"proto3\"; message Response { string value = 1; }".to_string(),
        )
        .unwrap()
        .get_message_by_name("Response")
        .unwrap();
        let bytes = ProstMessageCodec::serialize_message(&GrpcUnaryResult {
            metadata: Default::default(),
            code: 14,
            message: Some("unavailable".to_string()),
            response_data: Some(grpc_unary_result::ResponseData::JsonBody(
                r#"{"value":"ignored"}"#.to_string(),
            )),
        })
        .unwrap();

        let result = decode_result(
            &bytes,
            &descriptor,
            GrpcMethodKind::Unary,
            serde_json::json!({"jobId":"1"}),
        )
        .unwrap();
        let content = result.structured_content.as_ref().unwrap();
        assert!(result.is_error.unwrap_or_default());
        assert_eq!(
            content.pointer("/error/stage"),
            Some(&serde_json::json!("execution"))
        );
        assert!(content.pointer("/output/body").is_none());
        assert_eq!(
            content.pointer("/executionInfo/jobId"),
            Some(&serde_json::json!("1"))
        );
    }

    #[tokio::test]
    async fn failed_job_result_is_preserved_as_an_execution_error() {
        let enqueued = EnqueuedFunction {
            job_id: Default::default(),
            runner_name: "GRPC".to_string(),
            result: None,
            raw_result: Some(JobResult {
                data: Some(JobResultData {
                    status: ResultStatus::FatalError as i32,
                    output: Some(ResultOutput {
                        items: b"upstream unavailable".to_vec(),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            is_streaming: false,
            result_handle: None,
            using: Some("unary".to_string()),
        };
        let error = collect_raw_result(enqueued).await.unwrap_err();
        assert!(error.to_string().contains("upstream unavailable"));
    }
}
