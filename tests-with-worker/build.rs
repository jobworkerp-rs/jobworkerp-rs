use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Compile the echo service used by the call.grpc integration test and emit a
    // file descriptor set so the test server can expose gRPC reflection.
    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .file_descriptor_set_path(out_dir.join("echo_descriptor.bin"))
        .compile_protos(&["protobuf/echo.proto"], &["protobuf"])
        .unwrap_or_else(|e| panic!("Failed to compile echo proto {e:?}"));

    Ok(())
}
