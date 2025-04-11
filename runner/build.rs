use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .file_descriptor_set_path(out_dir.join("jobworkerp_runner.bin")) // for reflection
        .type_attribute(
            ".",
            "#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]",
        )
        .compile_protos(
            &[
                // TODO proto file path
                "protobuf/jobworkerp/runner/common.proto",
                "protobuf/jobworkerp/runner/command_args.proto",
                "protobuf/jobworkerp/runner/command_result.proto",
                "protobuf/jobworkerp/runner/docker_runner.proto",
                "protobuf/jobworkerp/runner/docker_args.proto",
                "protobuf/jobworkerp/runner/grpc_unary_runner.proto",
                "protobuf/jobworkerp/runner/grpc_unary_args.proto",
                "protobuf/jobworkerp/runner/grpc_unary_result.proto",
                "protobuf/jobworkerp/runner/http_request_runner.proto",
                "protobuf/jobworkerp/runner/http_request_args.proto",
                "protobuf/jobworkerp/runner/http_request_result.proto",
                "protobuf/jobworkerp/runner/slack_runner.proto",
                "protobuf/jobworkerp/runner/slack_args.proto",
                "protobuf/jobworkerp/runner/slack_result.proto",
                "protobuf/jobworkerp/runner/python_command_result.proto",
                "protobuf/jobworkerp/runner/python_command_runner.proto",
                "protobuf/jobworkerp/runner/python_command_args.proto",
                "protobuf/jobworkerp/runner/workflow_result.proto",
                "protobuf/jobworkerp/runner/workflow_args.proto",
                "protobuf/jobworkerp/runner/reusable_workflow_args.proto",
                "protobuf/jobworkerp/runner/reusable_workflow_runner.proto",
                "protobuf/jobworkerp/runner/llm/runner.proto",
                "protobuf/jobworkerp/runner/llm/completion_result.proto",
                "protobuf/jobworkerp/runner/llm/completion_args.proto",
            ],
            &["../proto/protobuf/", "protobuf"],
        )
        .unwrap_or_else(|e| panic!("Failed to compile protos {:?}", e));

    Ok(())
}
