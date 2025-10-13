/// Test for ProtobufDescriptor::json_value_to_message() with oneof fields
///
/// This test verifies that ProtobufDescriptor can correctly convert JSON values
/// containing oneof fields (like PythonCommandRunnerSettings.requirements_spec)
/// into protobuf binary format.
use command_utils::protobuf::ProtobufDescriptor;
use jobworkerp_runner::jobworkerp::runner::{
    python_command_runner_settings, PythonCommandRunnerSettings,
};
use prost::Message;

#[test]
fn test_protobuf_descriptor_handles_oneof_fields() {
    println!("\n=== Testing ProtobufDescriptor with oneof fields ===\n");

    // Step 1: Create original settings with oneof field
    let original_settings = PythonCommandRunnerSettings {
        python_version: "3.12".to_string(),
        uv_path: None,
        requirements_spec: Some(python_command_runner_settings::RequirementsSpec::Packages(
            python_command_runner_settings::PackagesList {
                list: vec!["requests".to_string(), "numpy".to_string()],
            },
        )),
    };
    println!("1. Original settings:");
    println!("   python_version: {}", original_settings.python_version);
    println!("   uv_path: {:?}", original_settings.uv_path);
    if let Some(ref spec) = original_settings.requirements_spec {
        match spec {
            python_command_runner_settings::RequirementsSpec::Packages(ref p) => {
                println!("   requirements_spec.packages: {:?}", p.list);
            }
            python_command_runner_settings::RequirementsSpec::RequirementsUrl(ref u) => {
                println!("   requirements_spec.url: {}", u);
            }
        }
    }
    println!();

    // Step 2: Encode directly with prost (baseline)
    let mut direct_bytes = Vec::new();
    original_settings.encode(&mut direct_bytes).unwrap();
    println!("2. Direct protobuf encoding (baseline):");
    println!("   Bytes: {:?}", direct_bytes);
    println!("   Length: {} bytes\n", direct_bytes.len());

    // Step 3: Convert to JSON (simulating what script.rs does)
    let json_value = serde_json::to_value(&original_settings).unwrap();
    println!("3. JSON value (via serde_json):");
    println!("{}\n", serde_json::to_string_pretty(&json_value).unwrap());

    // Step 4: Load proto descriptor
    let proto_content =
        include_str!("../../runner/protobuf/jobworkerp/runner/python_command_runner.proto");
    let descriptor =
        ProtobufDescriptor::new(&proto_content.to_string()).expect("Failed to parse proto file");
    let message_descriptor = descriptor
        .get_messages()
        .first()
        .expect("No messages found in proto file")
        .clone();

    println!("4. Proto descriptor loaded:");
    println!("   Message name: {}\n", message_descriptor.full_name());

    // Step 5: Convert JSON to protobuf using ProtobufDescriptor
    // This is what execute.rs:276 does in setup_runner_and_settings()
    let bytes_from_json = ProtobufDescriptor::json_value_to_message(
        message_descriptor,
        &json_value,
        true, // use_default
    )
    .expect("Failed to convert JSON to protobuf message");

    println!("5. Bytes from ProtobufDescriptor::json_value_to_message():");
    println!("   Bytes: {:?}", bytes_from_json);
    println!("   Length: {} bytes\n", bytes_from_json.len());

    // Step 6: Decode back to struct
    let decoded_settings = PythonCommandRunnerSettings::decode(bytes_from_json.as_slice())
        .expect("Failed to decode protobuf bytes");

    println!("6. Decoded settings:");
    println!("   python_version: {}", decoded_settings.python_version);
    println!("   uv_path: {:?}", decoded_settings.uv_path);
    if let Some(ref spec) = decoded_settings.requirements_spec {
        match spec {
            python_command_runner_settings::RequirementsSpec::Packages(ref p) => {
                println!("   requirements_spec.packages: {:?}", p.list);
            }
            python_command_runner_settings::RequirementsSpec::RequirementsUrl(ref u) => {
                println!("   requirements_spec.url: {}", u);
            }
        }
    } else {
        println!("   requirements_spec: None ❌");
    }
    println!();

    // Step 7: Verification
    println!("=== Verification ===");

    assert_eq!(
        decoded_settings.python_version, "3.12",
        "python_version should be preserved"
    );
    println!("✅ python_version preserved");

    assert!(
        decoded_settings.requirements_spec.is_some(),
        "❌ CRITICAL: requirements_spec is None! ProtobufDescriptor lost the oneof field."
    );
    println!("✅ requirements_spec is Some");

    match &decoded_settings.requirements_spec {
        Some(python_command_runner_settings::RequirementsSpec::Packages(packages)) => {
            assert_eq!(packages.list.len(), 2, "Should have 2 packages");
            assert_eq!(packages.list[0], "requests");
            assert_eq!(packages.list[1], "numpy");
            println!("✅ requirements_spec.packages preserved correctly");
            println!("✅ Package count: {}", packages.list.len());
            println!("✅ Packages: {:?}", packages.list);
        }
        Some(python_command_runner_settings::RequirementsSpec::RequirementsUrl(_)) => {
            panic!("❌ Wrong variant: expected Packages, got RequirementsUrl");
        }
        None => {
            panic!("❌ requirements_spec is None (should have been caught by assert above)");
        }
    }

    println!("\n=== Test Result: ✅ PASS ===");
    println!("ProtobufDescriptor correctly handles oneof fields\n");
}
