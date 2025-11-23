fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .type_attribute(
            ".",
            "#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]",
        )
        .compile_protos(
            &[
                "protobuf/mistral_runner.proto",
                "protobuf/mistral_chat_args.proto",
                "protobuf/mistral_result.proto",
            ],
            &["protobuf"],
        )?;
    Ok(())
}
