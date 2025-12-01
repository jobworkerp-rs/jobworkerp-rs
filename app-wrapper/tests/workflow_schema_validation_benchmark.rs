use std::time::Instant;

/// Benchmark test to measure workflow schema validator initialization time
/// This test helps identify if jsonschema::draft202012 initialization is causing delays
#[test]
fn benchmark_workflow_schema_validator_initialization() {
    println!("🔍 Starting workflow schema validator initialization benchmark");
    println!("================================================");

    // Step 1: Load and parse YAML schema
    println!("\n📄 Step 1: Loading YAML schema...");
    let schema_content = include_str!("../../runner/schema/workflow.yaml");
    println!("   Schema size: {} bytes", schema_content.len());
    println!("   Schema lines: {}", schema_content.lines().count());

    let yaml_parse_start = Instant::now();
    let schema: serde_json::Value =
        serde_yaml::from_str(schema_content).expect("Failed to parse workflow schema YAML");
    let yaml_parse_duration = yaml_parse_start.elapsed();
    println!("   ✅ YAML parsing took: {:?}", yaml_parse_duration);

    // Step 2: Initialize jsonschema validator
    println!("\n🔧 Step 2: Initializing jsonschema validator...");
    println!("   Using: jsonschema::draft202012::new()");

    let validator_init_start = Instant::now();
    let validator_result = jsonschema::draft202012::new(&schema);
    let validator_init_duration = validator_init_start.elapsed();

    println!(
        "   ✅ Validator initialization took: {:?}",
        validator_init_duration
    );

    assert!(
        validator_result.is_ok(),
        "Validator initialization should succeed"
    );
    let validator = validator_result.unwrap();

    // Step 3: Perform actual validation on a sample workflow
    println!("\n✔️  Step 3: Testing validation performance...");

    let sample_workflow = serde_json::json!({
        "document": {
            "dsl": "0.0.1",
            "namespace": "benchmark-test",
            "name": "validation-benchmark",
            "version": "1.0.0",
            "title": "Validation Benchmark Test"
        },
        "input": {},
        "do": [{
            "test_task": {
                "run": {
                    "runner": {
                        "name": "COMMAND",
                        "arguments": {
                            "command": "echo",
                            "args": ["test"]
                        }
                    }
                }
            }
        }]
    });

    let validation_start = Instant::now();
    let is_valid = validator.is_valid(&sample_workflow);
    let validation_duration = validation_start.elapsed();

    println!(
        "   Sample workflow validation took: {:?}",
        validation_duration
    );
    println!(
        "   Validation result: {}",
        if is_valid { "VALID" } else { "INVALID" }
    );

    // Step 4: Summary and thresholds
    println!("\n📊 Performance Summary:");
    println!("   - YAML parsing:            {:?}", yaml_parse_duration);
    println!(
        "   - Validator initialization: {:?}",
        validator_init_duration
    );
    println!("   - Single validation:        {:?}", validation_duration);
    println!(
        "   - Total time:               {:?}",
        yaml_parse_duration + validator_init_duration + validation_duration
    );

    // Performance assertions
    println!("\n🎯 Performance Analysis:");

    if validator_init_duration.as_secs() < 1 {
        println!("   ✅ FAST: Initialization < 1 second");
        println!("   → Recommendation: Use LazyLock with direct initialization");
    } else if validator_init_duration.as_secs() < 10 {
        println!("   ⚠️  MODERATE: Initialization 1-10 seconds");
        println!("   → Recommendation: Initialize at application startup");
    } else if validator_init_duration.as_secs() < 60 {
        println!("   ❌ SLOW: Initialization 10-60 seconds");
        println!("   → Recommendation: Use spawn_blocking or lightweight validator");
    } else {
        println!("   🚨 VERY SLOW: Initialization > 60 seconds");
        println!("   → Recommendation: Replace with lightweight validation");
    }

    println!("\n================================================");
    println!("✅ Benchmark completed successfully");

    // Fail the test if initialization takes more than 5 seconds
    // This helps catch performance regressions
    assert!(
        validator_init_duration.as_secs() < 5,
        "Validator initialization took too long: {:?}. Expected < 5 seconds",
        validator_init_duration
    );
}

/// Test to verify that multiple initializations have consistent performance
#[test]
fn benchmark_multiple_initializations() {
    println!("🔄 Testing multiple validator initializations");

    let schema_content = include_str!("../../runner/schema/workflow.yaml");
    let schema: serde_json::Value =
        serde_yaml::from_str(schema_content).expect("Failed to parse workflow schema YAML");

    const ITERATIONS: usize = 3;
    let mut durations = Vec::with_capacity(ITERATIONS);

    for i in 1..=ITERATIONS {
        println!("\n   Iteration {}/{}:", i, ITERATIONS);
        let start = Instant::now();
        let validator = jsonschema::draft202012::new(&schema);
        let duration = start.elapsed();

        assert!(
            validator.is_ok(),
            "Validator should initialize successfully"
        );
        durations.push(duration);
        println!("   Duration: {:?}", duration);
    }

    let avg_duration = durations.iter().sum::<std::time::Duration>() / ITERATIONS as u32;
    let max_duration = durations.iter().max().unwrap();
    let min_duration = durations.iter().min().unwrap();

    println!("\n📊 Multiple Initialization Statistics:");
    println!("   - Average: {:?}", avg_duration);
    println!("   - Min:     {:?}", min_duration);
    println!("   - Max:     {:?}", max_duration);
    println!(
        "   - Variance: {:?}",
        max_duration.saturating_sub(*min_duration)
    );

    // Consistent performance check
    let variance = max_duration
        .as_millis()
        .saturating_sub(min_duration.as_millis());
    if variance < 100 {
        println!("   ✅ Performance is consistent (variance < 100ms)");
    } else {
        println!("   ⚠️  Performance varies by {}ms", variance);
    }
}
