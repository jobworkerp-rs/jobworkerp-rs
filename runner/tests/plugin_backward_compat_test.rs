use anyhow::Result;
use jobworkerp_runner::runner::plugins::Plugins;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use jsonschema::Validator;
use std::collections::HashMap;

const TEST_PLUGIN_DIR: &str = "./target/debug,../target/debug,../target/release,./target/release";

#[tokio::test]
async fn test_load_legacy_plugin() -> Result<()> {
    let plugins = Plugins::new();
    let loaded = plugins.load_plugin_files(TEST_PLUGIN_DIR).await;

    // Find LegacyCompat plugin
    let legacy_plugin = loaded
        .iter()
        .find(|p| p.name == "LegacyCompat")
        .expect("LegacyCompat plugin should be loaded");

    assert_eq!(legacy_plugin.name, "LegacyCompat");
    assert!(legacy_plugin
        .description
        .contains("Legacy compatibility test plugin"));

    Ok(())
}

#[tokio::test]
async fn test_legacy_plugin_method_proto_map_conversion() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;

    let runner_loader = plugins.runner_plugins();
    let loader = runner_loader.read().await;

    let plugin_wrapper = loader
        .find_plugin_runner_by_name("LegacyCompat")
        .await
        .expect("LegacyCompat plugin should be found");

    // Verify method_proto_map() conversion
    let method_proto_map = plugin_wrapper.method_proto_map();

    // Should have DEFAULT_METHOD_NAME entry
    assert!(
        method_proto_map.contains_key(proto::DEFAULT_METHOD_NAME),
        "Legacy plugin should have '{}' method",
        proto::DEFAULT_METHOD_NAME
    );

    let method_schema = method_proto_map
        .get(proto::DEFAULT_METHOD_NAME)
        .expect("Method schema should exist");

    // Verify schema content
    assert!(!method_schema.args_proto.is_empty());
    assert!(!method_schema.result_proto.is_empty());
    assert_eq!(
        method_schema.output_type,
        proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32
    );

    Ok(())
}

#[tokio::test]
async fn test_legacy_plugin_method_json_schema_map_conversion() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;

    let runner_loader = plugins.runner_plugins();
    let loader = runner_loader.read().await;

    let plugin_wrapper = loader
        .find_plugin_runner_by_name("LegacyCompat")
        .await
        .expect("LegacyCompat plugin should be found");

    // Verify method_json_schema_map() conversion
    let method_json_schema_map = plugin_wrapper.method_json_schema_map();

    // Should have DEFAULT_METHOD_NAME entry
    assert!(
        method_json_schema_map.contains_key(proto::DEFAULT_METHOD_NAME),
        "Legacy plugin should have '{}' method in JSON schema map",
        proto::DEFAULT_METHOD_NAME
    );

    let json_schema = method_json_schema_map
        .get(proto::DEFAULT_METHOD_NAME)
        .expect("JSON schema should exist");

    // Verify JSON schema content
    assert!(!json_schema.args_schema.is_empty());
    assert!(json_schema.result_schema.is_some());

    // Verify args_schema is valid JSON Schema
    let args_value: serde_json::Value =
        serde_json::from_str(&json_schema.args_schema).expect("args_schema should be valid JSON");
    assert!(
        args_value.is_object(),
        "args_schema should be a JSON object"
    );

    // Validate args_schema is a valid JSON Schema by compiling it
    let args_validator =
        Validator::new(&args_value).expect("args_schema should be a valid JSON Schema");

    // Verify it can validate a sample instance
    let sample_args = serde_json::json!({
        "test_input": "sample",
        "test_number": 42
    });
    assert!(
        args_validator.is_valid(&sample_args),
        "args_schema should validate valid input"
    );

    // Verify result_schema is valid JSON Schema
    if let Some(result_schema) = &json_schema.result_schema {
        let result_value: serde_json::Value =
            serde_json::from_str(result_schema).expect("result_schema should be valid JSON");
        assert!(
            result_value.is_object(),
            "result_schema should be a JSON object"
        );

        // Validate result_schema is a valid JSON Schema by compiling it
        let result_validator =
            Validator::new(&result_value).expect("result_schema should be a valid JSON Schema");

        // Verify it can validate a sample instance
        let sample_result = serde_json::json!({
            "result_message": "test",
            "success": true
        });
        assert!(
            result_validator.is_valid(&sample_result),
            "result_schema should validate valid output"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_new_plugin_hello_method_proto_map() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;

    let runner_loader = plugins.runner_plugins();
    let loader = runner_loader.read().await;

    let plugin_wrapper = loader
        .find_plugin_runner_by_name("HelloPlugin")
        .await
        .expect("HelloPlugin should be found");

    // Verify method_proto_map() is directly used (not auto-converted)
    let method_proto_map = plugin_wrapper.method_proto_map();

    // HelloPlugin implements method_proto_map() explicitly
    assert!(
        method_proto_map.contains_key(proto::DEFAULT_METHOD_NAME),
        "HelloPlugin should have '{}' method",
        proto::DEFAULT_METHOD_NAME
    );

    let method_schema = method_proto_map
        .get(proto::DEFAULT_METHOD_NAME)
        .expect("Method schema should exist");

    // Verify HelloPlugin's specific schema
    assert!(method_schema.args_proto.contains("HelloArgs"));
    assert!(method_schema.result_proto.contains("HelloRunnerResult"));

    Ok(())
}

#[tokio::test]
async fn test_execute_legacy_plugin_without_using() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;

    let runner_loader = plugins.runner_plugins();
    let loader = runner_loader.read().await;

    let mut plugin_wrapper = loader
        .find_plugin_runner_by_name("LegacyCompat")
        .await
        .expect("LegacyCompat plugin should be found");

    // Load plugin
    plugin_wrapper.load(vec![]).await?;

    // Execute without 'using' parameter
    let test_input = br#"test_input"#;
    let (result, _metadata) = plugin_wrapper.run(test_input, HashMap::new(), None).await;

    // Should succeed
    assert!(result.is_ok(), "Legacy plugin should execute successfully");

    Ok(())
}

#[tokio::test]
async fn test_execute_legacy_plugin_with_using_warning() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;

    let runner_loader = plugins.runner_plugins();
    let loader = runner_loader.read().await;

    let mut plugin_wrapper = loader
        .find_plugin_runner_by_name("LegacyCompat")
        .await
        .expect("LegacyCompat plugin should be found");

    // Load plugin
    plugin_wrapper.load(vec![]).await?;

    // Execute with 'using' parameter (should be ignored with warning)
    let test_input = br#"test_input"#;
    let (result, _metadata) = plugin_wrapper
        .run(test_input, HashMap::new(), Some("foo"))
        .await;

    // Should succeed (using parameter ignored)
    assert!(
        result.is_ok(),
        "Legacy plugin should execute successfully even with 'using' parameter"
    );

    Ok(())
}

#[tokio::test]
async fn test_both_plugins_loadable() -> Result<()> {
    let plugins = Plugins::new();
    let loaded = plugins.load_plugin_files(TEST_PLUGIN_DIR).await;

    // Verify both legacy and new plugins are loaded
    let legacy_count = loaded.iter().filter(|p| p.name == "LegacyCompat").count();
    let hello_count = loaded.iter().filter(|p| p.name == "HelloPlugin").count();

    assert_eq!(legacy_count, 1, "LegacyCompat plugin should be loaded once");
    assert_eq!(hello_count, 1, "HelloPlugin should be loaded once");

    Ok(())
}

#[tokio::test]
async fn test_legacy_plugin_settings_schema() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;

    let runner_loader = plugins.runner_plugins();
    let loader = runner_loader.read().await;

    let plugin_wrapper = loader
        .find_plugin_runner_by_name("LegacyCompat")
        .await
        .expect("LegacyCompat plugin should be found");

    // Verify settings_schema() returns valid JSON
    let settings_schema = plugin_wrapper.settings_schema();
    assert!(
        !settings_schema.is_empty(),
        "settings_schema should not be empty"
    );

    let settings_value: serde_json::Value =
        serde_json::from_str(&settings_schema).expect("settings_schema should be valid JSON");
    assert!(
        settings_value.is_object(),
        "settings_schema should be a JSON object"
    );
    assert!(
        settings_value.get("$schema").is_some(),
        "settings_schema should have $schema field"
    );

    // Validate settings_schema is a valid JSON Schema by compiling it
    let settings_validator =
        Validator::new(&settings_value).expect("settings_schema should be a valid JSON Schema");

    // Verify it can validate a sample settings instance
    let sample_settings = serde_json::json!({
        "name": "test_settings"
    });
    assert!(
        settings_validator.is_valid(&sample_settings),
        "settings_schema should validate valid settings"
    );

    Ok(())
}

#[tokio::test]
async fn test_multi_method_plugin_settings_schema() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;

    let runner_loader = plugins.runner_plugins();
    let loader = runner_loader.read().await;

    let plugin_wrapper = loader
        .find_plugin_runner_by_name("HelloPlugin")
        .await
        .expect("HelloPlugin should be found");

    // Verify settings_schema() returns valid JSON
    let settings_schema = plugin_wrapper.settings_schema();
    assert!(
        !settings_schema.is_empty(),
        "settings_schema should not be empty"
    );

    let settings_value: serde_json::Value =
        serde_json::from_str(&settings_schema).expect("settings_schema should be valid JSON");
    assert!(
        settings_value.is_object(),
        "settings_schema should be a JSON object"
    );
    assert!(
        settings_value.get("$schema").is_some(),
        "settings_schema should have $schema field"
    );

    // Validate settings_schema is a valid JSON Schema by compiling it
    let settings_validator =
        Validator::new(&settings_value).expect("settings_schema should be a valid JSON Schema");

    // Verify it can validate a sample settings instance
    let sample_settings = serde_json::json!({
        "name": "test_settings"
    });
    assert!(
        settings_validator.is_valid(&sample_settings),
        "settings_schema should validate valid settings"
    );

    Ok(())
}
