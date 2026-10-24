use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("../proto/protobuf");
    let files = [
        proto_root.join("jobworkerp/service/common.proto"),
        proto_root.join("jobworkerp/service/runner.proto"),
        proto_root.join("jobworkerp/service/worker.proto"),
        proto_root.join("jobworkerp/service/job.proto"),
    ];

    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
    }
    println!("cargo:rerun-if-changed={}", proto_root.display());

    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&files, &[proto_root])?;

    Ok(())
}
