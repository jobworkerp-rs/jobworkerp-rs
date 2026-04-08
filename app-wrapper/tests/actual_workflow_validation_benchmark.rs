use std::time::Instant;

/// Benchmark test for the actual problematic workflow file
/// This measures the validation time for claude-code-collection-pipeline.yaml
/// which was reported to cause multi-minute delays
#[test]
#[ignore = "depends on local file path"]
fn benchmark_actual_workflow_validation() {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    println!("🔍 Benchmarking actual workflow file validation");
    println!("================================================");
    println!("Target: claude-code-collection-pipeline.yaml");

    // Step 1: Load the workflow schema for validation
    println!("\n📄 Step 1: Loading workflow schema...");
    let schema_content = include_str!("../../runner/schema/workflow.yaml");

    let yaml_parse_start = Instant::now();
    let schema: serde_json::Value =
        serde_yaml::from_str(schema_content).expect("Failed to parse workflow schema YAML");
    let yaml_parse_duration = yaml_parse_start.elapsed();
    println!("   ✅ Schema YAML parsing: {:?}", yaml_parse_duration);

    // Step 2: Initialize validator
    println!("\n🔧 Step 2: Initializing validator...");
    let validator_init_start = Instant::now();
    let validator = jsonschema::draft202012::new(&schema).expect("Failed to create validator");
    let validator_init_duration = validator_init_start.elapsed();
    println!(
        "   ✅ Validator initialization: {:?}",
        validator_init_duration
    );

    // Step 3: Load the actual problematic workflow file
    println!("\n📋 Step 3: Loading actual workflow file...");
    let workflow_file_path = "/home/sutr/mnt/works/rust/jobworkerp-rs/message-vectordb/docs/workflows/claude-code-collection-pipeline.yaml";

    let workflow_load_start = Instant::now();
    let workflow_content =
        std::fs::read_to_string(workflow_file_path).expect("Failed to read workflow file");
    let workflow_load_duration = workflow_load_start.elapsed();

    println!("   File size: {} bytes", workflow_content.len());
    println!("   File lines: {}", workflow_content.lines().count());
    println!("   ✅ File read time: {:?}", workflow_load_duration);

    // Step 4: Parse workflow YAML
    println!("\n🔄 Step 4: Parsing workflow YAML...");
    let workflow_parse_start = Instant::now();
    let workflow_json: serde_json::Value =
        serde_yaml::from_str(&workflow_content).expect("Failed to parse workflow YAML");
    let workflow_parse_duration = workflow_parse_start.elapsed();
    println!("   ✅ Workflow YAML parsing: {:?}", workflow_parse_duration);

    // Step 5: Validate the workflow
    println!("\n✔️  Step 5: Validating workflow against schema...");
    let validation_start = Instant::now();
    let is_valid = validator.is_valid(&workflow_json);
    let validation_duration = validation_start.elapsed();

    if is_valid {
        println!("   ✅ Validation PASSED");
        println!("   ⏱️  Validation time: {:?}", validation_duration);
    } else {
        println!("   ❌ Validation FAILED");
        println!("   ⏱️  Validation time: {:?}", validation_duration);
        println!("\n   Validation errors:");
        for (i, error) in validator.iter_errors(&workflow_json).enumerate() {
            println!(
                "     {}. Path: {}, Error: {}",
                i + 1,
                error.instance_path(),
                error
            );
            if i >= 10 {
                println!("     ... (showing first 10 errors)");
                break;
            }
        }
    }

    // Step 6: Summary
    println!("\n📊 Complete Workflow Validation Summary:");
    println!("   - Schema YAML parsing:       {:?}", yaml_parse_duration);
    println!(
        "   - Validator initialization:   {:?}",
        validator_init_duration
    );
    println!(
        "   - Workflow file read:         {:?}",
        workflow_load_duration
    );
    println!(
        "   - Workflow YAML parsing:      {:?}",
        workflow_parse_duration
    );
    println!("   - Workflow validation:        {:?}", validation_duration);
    println!("   ----------------------------------------");
    println!(
        "   - Total time:                 {:?}",
        yaml_parse_duration
            + validator_init_duration
            + workflow_load_duration
            + workflow_parse_duration
            + validation_duration
    );

    // Step 7: Performance analysis
    println!("\n🎯 Performance Analysis:");

    let total_validation_time = validation_duration;

    if total_validation_time.as_secs() < 1 {
        println!("   ✅ FAST: Validation < 1 second");
        println!("   → Validation is NOT the bottleneck");
    } else if total_validation_time.as_secs() < 10 {
        println!("   ⚠️  MODERATE: Validation 1-10 seconds");
        println!("   → Validation may contribute to delays");
    } else if total_validation_time.as_secs() < 60 {
        println!("   ❌ SLOW: Validation 10-60 seconds");
        println!("   → Validation is a significant bottleneck");
    } else {
        println!("   🚨 VERY SLOW: Validation > 60 seconds");
        println!("   → Validation is the primary bottleneck");
    }

    println!("\n================================================");

    // Assert that validation doesn't take more than 5 seconds
    assert!(
        validation_duration.as_secs() < 5,
        "Validation took too long: {:?}. This workflow should validate quickly.",
        validation_duration
    );
}

/// Test the complete workflow loading process including validation
#[test]
#[ignore]
fn benchmark_complete_workflow_loading() {
    println!("🔄 Benchmarking complete workflow loading process");

    let workflow_file_path = "/home/sutr/mnt/works/rust/jobworkerp-rs/message-vectordb/docs/workflows/claude-code-collection-pipeline.yaml";

    // Simulate the actual workflow loading path
    println!("\n📥 Simulating actual WorkflowLoader path...");

    let total_start = Instant::now();

    // This mimics what happens in WorkflowLoader::load_workflow
    let load_start = Instant::now();
    let workflow_content =
        std::fs::read_to_string(workflow_file_path).expect("Failed to read workflow file");
    let load_duration = load_start.elapsed();
    println!("   File read: {:?}", load_duration);

    let parse_start = Instant::now();
    let workflow_json: serde_json::Value =
        serde_yaml::from_str(&workflow_content).expect("Failed to parse workflow");
    let parse_duration = parse_start.elapsed();
    println!("   YAML parse: {:?}", parse_duration);

    // Load schema and create validator (happens once via LazyLock)
    let validator_start = Instant::now();
    let schema_content = include_str!("../../runner/schema/workflow.yaml");
    let schema: serde_json::Value =
        serde_yaml::from_str(schema_content).expect("Failed to parse schema");
    let validator = jsonschema::draft202012::new(&schema).expect("Failed to create validator");
    let validator_duration = validator_start.elapsed();
    println!("   Validator init: {:?}", validator_duration);

    let validate_start = Instant::now();
    let is_valid = validator.is_valid(&workflow_json);
    let validate_duration = validate_start.elapsed();
    println!("   Validation: {:?}", validate_duration);

    let total_duration = total_start.elapsed();

    println!("\n📊 Complete Loading Process:");
    println!("   - File read:        {:?}", load_duration);
    println!("   - YAML parse:       {:?}", parse_duration);
    println!("   - Validator init:   {:?}", validator_duration);
    println!("   - Validation:       {:?}", validate_duration);
    println!("   ----------------------------------------");
    println!("   - Total:            {:?}", total_duration);
    println!("   - Validation valid: {}", is_valid);

    if total_duration.as_secs() < 2 {
        println!("\n   ✅ Complete loading process is fast (< 2s)");
        println!("   → Bottleneck is elsewhere (likely in workflow execution)");
    } else {
        println!(
            "\n   ⚠️  Complete loading process took {} seconds",
            total_duration.as_secs()
        );
    }
}
