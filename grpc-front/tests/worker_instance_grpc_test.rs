//! Worker Instance gRPC Service Integration Tests
//!
//! Tests for FindChannelList aggregation and other gRPC service behaviors.
//! These tests verify the channel aggregation logic used in WorkerInstanceGrpcImpl.

use std::collections::HashMap;

use proto::jobworkerp::data::{
    ChannelConfig, WorkerInstance, WorkerInstanceData, WorkerInstanceId,
};

/// Aggregate worker instance channel information
///
/// This is a pure function that replicates the aggregation logic
/// from WorkerInstanceGrpcImpl::find_channel_list for testing.
fn aggregate_instance_channels(
    instances: &[WorkerInstance],
    worker_counts: &HashMap<String, i64>,
) -> Vec<ChannelInfo> {
    let mut channel_map: HashMap<String, (u32, usize)> = HashMap::new();

    for instance in instances {
        if let Some(data) = &instance.data {
            for channel in &data.channels {
                let entry = channel_map.entry(channel.name.clone()).or_insert((0, 0));
                entry.0 += channel.concurrency;
                entry.1 += 1;
            }
        }
    }

    channel_map
        .into_iter()
        .map(|(name, (total_concurrency, active_instances))| {
            let display_name = if name.is_empty() {
                "[default]".to_string()
            } else {
                name.clone()
            };
            let worker_count = *worker_counts.get(&name).unwrap_or(&0);

            ChannelInfo {
                name: display_name,
                total_concurrency,
                active_instances: active_instances as i32,
                worker_count,
            }
        })
        .collect()
}

/// Channel information structure for testing
#[derive(Debug, Clone, PartialEq)]
struct ChannelInfo {
    name: String,
    total_concurrency: u32,
    active_instances: i32,
    worker_count: i64,
}

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
    let worker_counts = HashMap::new();

    let result = aggregate_instance_channels(&instances, &worker_counts);

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
    let worker_counts = HashMap::new();

    let result = aggregate_instance_channels(&instances, &worker_counts);

    assert_eq!(result.len(), 2, "Should have 2 channels");

    let default_channel = result.iter().find(|c| c.name == "[default]");
    assert!(default_channel.is_some(), "Should have [default] channel");
    let default_channel = default_channel.unwrap();
    assert_eq!(default_channel.total_concurrency, 4);
    assert_eq!(default_channel.active_instances, 1);

    let priority_channel = result.iter().find(|c| c.name == "priority");
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

    let mut worker_counts = HashMap::new();
    worker_counts.insert("".to_string(), 10);
    worker_counts.insert("priority".to_string(), 5);

    let result = aggregate_instance_channels(&instances, &worker_counts);

    assert_eq!(result.len(), 2, "Should have 2 unique channels");

    let default_channel = result.iter().find(|c| c.name == "[default]").unwrap();
    assert_eq!(
        default_channel.total_concurrency,
        4 + 8 + 2,
        "Default channel concurrency should be sum"
    );
    assert_eq!(
        default_channel.active_instances, 3,
        "Default channel should have 3 active instances"
    );
    assert_eq!(
        default_channel.worker_count, 10,
        "Worker count should match"
    );

    let priority_channel = result.iter().find(|c| c.name == "priority").unwrap();
    assert_eq!(
        priority_channel.total_concurrency,
        2 + 4,
        "Priority channel concurrency should be sum"
    );
    assert_eq!(
        priority_channel.active_instances, 2,
        "Priority channel should have 2 active instances"
    );
    assert_eq!(
        priority_channel.worker_count, 5,
        "Worker count should match"
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
    let worker_counts = HashMap::new();

    let result = aggregate_instance_channels(&instances, &worker_counts);

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
    let worker_counts = HashMap::new();

    let result = aggregate_instance_channels(&instances, &worker_counts);

    assert!(
        result.is_empty(),
        "Instance with None data should not contribute to aggregation"
    );
}

/// Test: Default channel name display
/// Verifies: empty string channel name is displayed as "[default]"
#[test]
fn test_default_channel_display_name() {
    let instances = vec![create_test_instance(1, "192.168.1.1", vec![("", 4)])];
    let worker_counts = HashMap::new();

    let result = aggregate_instance_channels(&instances, &worker_counts);

    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].name, "[default]",
        "Empty channel name should be displayed as [default]"
    );
}

/// Test: Worker counts integration
/// Verifies: worker counts are correctly associated with channels
#[test]
fn test_worker_counts_integration() {
    let instances = vec![
        create_test_instance(1, "192.168.1.1", vec![("", 4)]),
        create_test_instance(2, "192.168.1.2", vec![("batch", 8)]),
    ];

    let mut worker_counts = HashMap::new();
    worker_counts.insert("".to_string(), 100);
    worker_counts.insert("batch".to_string(), 50);
    worker_counts.insert("unused".to_string(), 25); // Not used by any instance

    let result = aggregate_instance_channels(&instances, &worker_counts);

    assert_eq!(result.len(), 2);

    let default_channel = result.iter().find(|c| c.name == "[default]").unwrap();
    assert_eq!(default_channel.worker_count, 100);

    let batch_channel = result.iter().find(|c| c.name == "batch").unwrap();
    assert_eq!(batch_channel.worker_count, 50);
}

/// Test: Missing worker count defaults to zero
/// Verifies: channels without worker count entry default to 0
#[test]
fn test_missing_worker_count_defaults_to_zero() {
    let instances = vec![create_test_instance(1, "192.168.1.1", vec![("unknown", 4)])];
    let worker_counts = HashMap::new(); // Empty worker counts

    let result = aggregate_instance_channels(&instances, &worker_counts);

    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].worker_count, 0,
        "Missing worker count should default to 0"
    );
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

    let worker_counts = HashMap::new();

    let result = aggregate_instance_channels(&instances, &worker_counts);

    let default_channel = result.iter().find(|c| c.name == "[default]").unwrap();
    assert_eq!(
        default_channel.total_concurrency,
        4 * 100,
        "Total concurrency should be sum of all instances"
    );
    assert_eq!(
        default_channel.active_instances, 100,
        "Should have 100 active instances"
    );

    let priority_channel = result.iter().find(|c| c.name == "priority").unwrap();
    assert_eq!(priority_channel.total_concurrency, 2 * 100);
    assert_eq!(priority_channel.active_instances, 100);
}
