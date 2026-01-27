//! Worker Instance gRPC Service Integration Tests
//!
//! Tests for FindChannelList aggregation and other gRPC service behaviors.
//! These tests verify the channel aggregation logic from the repository layer.

use infra::infra::worker_instance::{ChannelAggregation, aggregate_instance_channels};
use proto::jobworkerp::data::{
    ChannelConfig, WorkerInstance, WorkerInstanceData, WorkerInstanceId,
};

fn create_test_instance(id: i64, ip: &str, channels: Vec<(&str, u32)>) -> WorkerInstance {
    let now = command_utils::util::datetime::now_millis();
    WorkerInstance {
        id: Some(WorkerInstanceId { value: id }),
        data: Some(WorkerInstanceData {
            ip_address: ip.to_string(),
            hostname: Some(format!("host-{}", id)),
            channels: channels
                .into_iter()
                .map(|(name, concurrency)| ChannelConfig {
                    name: name.to_string(),
                    concurrency,
                })
                .collect(),
            registered_at: now,
            last_heartbeat: now,
        }),
    }
}

/// Test: Empty instance list aggregation
/// Verifies: empty input produces empty output
#[test]
fn test_aggregate_instance_channels_empty() {
    let instances: Vec<WorkerInstance> = vec![];

    let result = aggregate_instance_channels(&instances);

    assert!(
        result.is_empty(),
        "Empty instances should produce empty result"
    );
}

/// Test: Single instance aggregation
/// Verifies: single instance with multiple channels
#[test]
fn test_aggregate_instance_channels_single() {
    let instances = vec![create_test_instance(
        1,
        "192.168.1.1",
        vec![("", 4), ("priority", 2)],
    )];

    let result = aggregate_instance_channels(&instances);

    assert_eq!(result.len(), 2, "Should have 2 channels");

    let default_channel = result.get("");
    assert!(default_channel.is_some(), "Should have default channel");
    let default_channel = default_channel.unwrap();
    assert_eq!(default_channel.total_concurrency, 4);
    assert_eq!(default_channel.active_instances, 1);

    let priority_channel = result.get("priority");
    assert!(priority_channel.is_some(), "Should have priority channel");
    let priority_channel = priority_channel.unwrap();
    assert_eq!(priority_channel.total_concurrency, 2);
    assert_eq!(priority_channel.active_instances, 1);
}

/// Test: Multiple instances aggregation
/// Verifies: multiple instances with overlapping channels
#[test]
fn test_aggregate_instance_channels_multiple() {
    let instances = vec![
        create_test_instance(1, "192.168.1.1", vec![("", 4), ("priority", 2)]),
        create_test_instance(2, "192.168.1.2", vec![("", 8), ("priority", 4)]),
        create_test_instance(3, "192.168.1.3", vec![("", 2)]), // Only default channel
    ];

    let result = aggregate_instance_channels(&instances);

    assert_eq!(result.len(), 2, "Should have 2 unique channels");

    let default_channel = result.get("").unwrap();
    assert_eq!(
        default_channel.total_concurrency,
        4 + 8 + 2,
        "Default channel concurrency should be sum"
    );
    assert_eq!(
        default_channel.active_instances, 3,
        "Default channel should have 3 active instances"
    );

    let priority_channel = result.get("priority").unwrap();
    assert_eq!(
        priority_channel.total_concurrency,
        2 + 4,
        "Priority channel concurrency should be sum"
    );
    assert_eq!(
        priority_channel.active_instances, 2,
        "Priority channel should have 2 active instances"
    );
}

/// Test: Instance with no channels
/// Verifies: instance without channel data is handled gracefully
#[test]
fn test_aggregate_instance_channels_no_channels() {
    let instances = vec![WorkerInstance {
        id: Some(WorkerInstanceId { value: 1 }),
        data: Some(WorkerInstanceData {
            ip_address: "192.168.1.1".to_string(),
            hostname: None,
            channels: vec![], // No channels
            registered_at: 0,
            last_heartbeat: 0,
        }),
    }];

    let result = aggregate_instance_channels(&instances);

    assert!(
        result.is_empty(),
        "Instance with no channels should not contribute to aggregation"
    );
}

/// Test: Instance with None data
/// Verifies: instance without data field is handled gracefully
#[test]
fn test_aggregate_instance_channels_none_data() {
    let instances = vec![WorkerInstance {
        id: Some(WorkerInstanceId { value: 1 }),
        data: None, // No data
    }];

    let result = aggregate_instance_channels(&instances);

    assert!(
        result.is_empty(),
        "Instance with None data should not contribute to aggregation"
    );
}

/// Test: Default channel name (empty string)
/// Verifies: empty string channel name is preserved as-is
#[test]
fn test_default_channel_key() {
    let instances = vec![create_test_instance(1, "192.168.1.1", vec![("", 4)])];

    let result = aggregate_instance_channels(&instances);

    assert_eq!(result.len(), 1);
    assert!(
        result.contains_key(""),
        "Empty channel name should be the key"
    );
}

/// Test: ChannelAggregation struct
/// Verifies: struct fields are correct
#[test]
fn test_channel_aggregation_struct() {
    let agg = ChannelAggregation {
        total_concurrency: 10,
        active_instances: 3,
    };

    assert_eq!(agg.total_concurrency, 10);
    assert_eq!(agg.active_instances, 3);

    // Test Default
    let default_agg = ChannelAggregation::default();
    assert_eq!(default_agg.total_concurrency, 0);
    assert_eq!(default_agg.active_instances, 0);
}

/// Test: Large scale aggregation
/// Verifies: aggregation works correctly with many instances
#[test]
fn test_large_scale_aggregation() {
    let mut instances = Vec::new();
    for i in 0..100 {
        instances.push(create_test_instance(
            i,
            &format!("192.168.1.{}", i % 256),
            vec![("", 4), ("priority", 2)],
        ));
    }

    let result = aggregate_instance_channels(&instances);

    let default_channel = result.get("").unwrap();
    assert_eq!(
        default_channel.total_concurrency,
        4 * 100,
        "Total concurrency should be sum of all instances"
    );
    assert_eq!(
        default_channel.active_instances, 100,
        "Should have 100 active instances"
    );

    let priority_channel = result.get("priority").unwrap();
    assert_eq!(priority_channel.total_concurrency, 2 * 100);
    assert_eq!(priority_channel.active_instances, 100);
}

/// Test: Integration with worker_counts (display name transformation)
/// Verifies: channel display name transformation works
#[test]
fn test_display_name_transformation() {
    let instances = vec![
        create_test_instance(1, "192.168.1.1", vec![("", 4)]),
        create_test_instance(2, "192.168.1.2", vec![("batch", 8)]),
    ];

    let result = aggregate_instance_channels(&instances);

    // Raw result uses original keys
    assert!(result.contains_key(""));
    assert!(result.contains_key("batch"));

    // Transformation to display name is done at gRPC layer, not in aggregation
    // This test verifies the raw data is correct
    let default_agg = result.get("").unwrap();
    assert_eq!(default_agg.total_concurrency, 4);

    let batch_agg = result.get("batch").unwrap();
    assert_eq!(batch_agg.total_concurrency, 8);
}
