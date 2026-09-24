use command_utils::protobuf::{ProtobufDescriptor, resolve::resolve_proto_imports};
use jobworkerp_runner::jobworkerp::runner as generated;
use serde_json::Value;

const SETTINGS_PROTO: &str = include_str!("../protobuf/jobworkerp/runner/sandbox_settings.proto");
const ARGS_PROTO: &str = include_str!("../protobuf/jobworkerp/runner/sandbox_args.proto");
const RESULT_PROTO: &str = include_str!("../protobuf/jobworkerp/runner/sandbox_result.proto");
const COMMON_PROTO: &str = include_str!("../protobuf/jobworkerp/runner/sandbox_common.proto");
const COMMON_IMPORT_PATH: &str = "jobworkerp/runner/sandbox_common.proto";

fn settings_schema() -> String {
    resolve_proto_imports(SETTINGS_PROTO, &[(COMMON_IMPORT_PATH, COMMON_PROTO)])
        .expect("settings imports must resolve")
}

fn args_schema() -> String {
    resolve_proto_imports(ARGS_PROTO, &[(COMMON_IMPORT_PATH, COMMON_PROTO)])
        .expect("args imports must resolve")
}

fn assert_is_independent_schema(schema: &str, primary_message: &str) {
    assert!(
        !schema
            .lines()
            .any(|line| line.trim_start().starts_with("import ")),
        "schema must not contain unresolved imports:\n{schema}"
    );

    let descriptor = ProtobufDescriptor::new(&schema.to_owned())
        .expect("schema must compile without access to other proto files");
    let first_message = descriptor
        .get_messages()
        .into_iter()
        .next()
        .expect("schema must define a message");
    assert_eq!(
        first_message.name(),
        primary_message,
        "primary message must be first in the independently compiled schema"
    );
}

fn assert_json_roundtrip(descriptor: &ProtobufDescriptor, message_name: &str, json: &str) {
    let message = descriptor
        .get_message_by_name(&format!("jobworkerp.runner.{message_name}"))
        .expect("manual example message must be present in the schema");
    let encoded = ProtobufDescriptor::json_to_message(message, json, false)
        .expect("manual JSON must encode as protobuf");
    let decoded = descriptor
        .get_message_by_name_from_bytes(message_name, &encoded)
        .or_else(|_| {
            descriptor.get_message_by_name_from_bytes(
                &format!("jobworkerp.runner.{message_name}"),
                &encoded,
            )
        })
        .expect("protobuf bytes must decode back to the example message");
    let roundtripped = ProtobufDescriptor::message_to_json_value_with_proto_names(&decoded)
        .expect("decoded message must convert to JSON");
    let mut expected = serde_json::from_str::<Value>(json).expect("fixture JSON must be valid");
    // Protobuf JSON represents uint64 fields as decimal strings.
    normalize_protobuf_u64_json(&mut expected);
    assert_eq!(
        expected, roundtripped,
        "protobuf roundtrip must preserve the manual JSON example"
    );
}

fn normalize_protobuf_u64_json(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                if matches!(
                    name.as_str(),
                    "default_exec_timeout_ms"
                        | "execution_time_ms"
                        | "idle_timeout_sec"
                        | "max_duration_sec"
                        | "timeout_ms"
                ) && let Some(number) = value.as_u64()
                {
                    *value = Value::String(number.to_string());
                }
                normalize_protobuf_u64_json(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize_protobuf_u64_json(value);
            }
        }
        _ => {}
    }
}

#[test]
fn settings_args_and_result_sources_compile_independently_with_primary_first() {
    assert_is_independent_schema(&settings_schema(), "SandboxRunnerSettings");
    assert_is_independent_schema(&args_schema(), "SandboxExecArgs");
    assert_is_independent_schema(RESULT_PROTO, "SandboxExecResult");
}

#[test]
fn generated_sandbox_types_are_in_the_runner_package_module() {
    // These type references guard the build.rs compile list and package module exposure.
    let _ = std::any::type_name::<generated::SandboxRunnerSettings>();
    let _ = std::any::type_name::<generated::SandboxExecArgs>();
    let _ = std::any::type_name::<generated::SandboxExecResult>();
    let _ = std::any::type_name::<generated::SandboxVmConfig>();
    let _ = std::any::type_name::<generated::SandboxNetworkConfig>();
    let _ = std::any::type_name::<generated::SandboxBindMount>();
    let _ = std::any::type_name::<generated::SandboxAllowedHostMount>();
    let _ = std::any::type_name::<generated::SandboxNetworkRule>();
    let _ = std::any::type_name::<generated::SandboxNetworkDestination>();
    let _ = std::any::type_name::<generated::sandbox_network_destination::Destination>();
    let _ = std::any::type_name::<generated::SandboxPortRange>();
    let _ = std::any::type_name::<generated::SandboxExecOutput>();
    let _ = std::any::type_name::<generated::SandboxExecExit>();
    let _ = std::any::type_name::<generated::sandbox_exec_result::Result>();
}

#[test]
fn manual_settings_and_args_json_examples_roundtrip_through_protobuf() {
    const SETTINGS_JSON: &str = r#"
    {
      "vm": {
        "image": "python:3.12",
        "cpus": 2,
        "memory_mib": 1024,
        "root_disk_mib": 8192,
        "working_dir": "/tmp",
        "idle_timeout_sec": 300
      },
      "allowed_images": ["python:3.12"],
      "default_exec_timeout_ms": 60000
    }
    "#;
    const ARGS_JSON: &str = r#"
    {
      "command": "python3",
      "args": ["-c", "import sys; print(sys.stdin.read().upper())"],
      "stdin": "aGVsbG8K",
      "vm": {"cpus": 1, "memory_mib": 512}
    }
    "#;
    const NETWORK_SETTINGS_JSON: &str = r#"
    {
      "vm": {
        "image": "python:3.12",
        "cpus": 2,
        "memory_mib": 1024,
        "root_disk_mib": 8192
      },
      "allowed_images": ["python:3.12"],
      "network": {
        "enabled": true,
        "max_tcp_connections": 64,
        "max_udp_connections": 64,
        "rules": [
          {"action": "allow", "destination": {"group": "public"}, "protocols": ["tcp"], "ports": [{"start": 443, "end": 443}]},
          {"action": "allow", "destination": {"group": "host"}, "protocols": ["tcp", "udp"], "ports": [{"start": 53, "end": 53}]}
        ]
      }
    }
    "#;

    let settings = ProtobufDescriptor::new(&settings_schema()).expect("settings schema compiles");
    let args = ProtobufDescriptor::new(&args_schema()).expect("args schema compiles");
    assert_json_roundtrip(&settings, "SandboxRunnerSettings", SETTINGS_JSON);
    assert_json_roundtrip(&args, "SandboxExecArgs", ARGS_JSON);
    assert_json_roundtrip(&settings, "SandboxRunnerSettings", NETWORK_SETTINGS_JSON);
}

#[test]
fn sandbox_result_output_and_exit_oneof_payloads_roundtrip() {
    const OUTPUT_JSON: &str = r#"{"output":{"stream":"stderr","data":"AP8="}}"#;
    const EXIT_JSON: &str =
        r#"{"exit":{"exit_code":7,"execution_time_ms":15,"sandbox_id":"sandbox-1"}}"#;
    let descriptor =
        ProtobufDescriptor::new(&RESULT_PROTO.to_owned()).expect("result schema compiles");

    assert_json_roundtrip(&descriptor, "SandboxExecResult", OUTPUT_JSON);
    assert_json_roundtrip(&descriptor, "SandboxExecResult", EXIT_JSON);
}

#[test]
fn optional_false_and_zero_values_survive_settings_roundtrip() {
    let json = r#"{
      "vm": {"image": "python:3.12", "cpus": 0, "memory_mib": 1024, "root_disk_mib": 8192},
      "allowed_images": ["python:3.12"],
      "network": {"enabled": false}
    }"#;
    let descriptor = ProtobufDescriptor::new(&settings_schema()).expect("settings schema compiles");

    assert_json_roundtrip(&descriptor, "SandboxRunnerSettings", json);
}
