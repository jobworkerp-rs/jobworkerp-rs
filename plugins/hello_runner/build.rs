use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .file_descriptor_set_path(out_dir.join("hello.bin")) // for reflection
        .type_attribute(
            ".",
            "#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]",
        )
        .compile_protos(
            &[
                "protobuf/hello_runner.proto",
                "protobuf/hello_job_args.proto",
                "protobuf/hello_result.proto",
            ],
            &["protobuf"],
        )
        .unwrap_or_else(|e| panic!("Failed to compile protos {:?}", e));

    Ok(())
}
