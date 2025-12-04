use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Compile plugin-specific proto
    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .file_descriptor_set_path(out_dir.join("mistral_runner.bin"))
        .type_attribute(
            ".",
            "#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]",
        )
        .compile_protos(&["protobuf/mistral_runner_settings.proto"], &["protobuf"])
        .unwrap_or_else(|e| panic!("Failed to compile protos {e:?}"));

    // Compile FunctionService gRPC client from main proto directory
    // Include all dependent proto files
    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)
        .build_client(true)
        .file_descriptor_set_path(out_dir.join("function_service.bin"))
        .compile_protos(
            &[
                "../../proto/protobuf/jobworkerp/data/common.proto",
                "../../proto/protobuf/jobworkerp/data/runner.proto",
                "../../proto/protobuf/jobworkerp/data/worker.proto",
                "../../proto/protobuf/jobworkerp/function/data/function.proto",
                "../../proto/protobuf/jobworkerp/function/service/function.proto",
            ],
            &["../../proto/protobuf"],
        )
        .unwrap_or_else(|e| panic!("Failed to compile function service proto {e:?}"));

    Ok(())
}
