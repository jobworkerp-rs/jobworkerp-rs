use crate::jobworkerp::data::{RetryPolicy, RetryType};
use crate::{RetryPolicyExt, calculate_direct_response_timeout_ms};

#[test]
fn test_no_retry_returns_job_timeout() {
    let policy = RetryPolicy {
        r#type: RetryType::None as i32,
        interval: 1000,
        max_interval: 60000,
        max_retry: 0,
        basis: 2.0,
    };

    let job_timeout = 1200000; // 20 minutes
    let total = policy.calculate_total_timeout_ms(job_timeout);

    assert_eq!(total, job_timeout);
}

#[test]
fn test_max_retry_zero_returns_job_timeout() {
    let policy = RetryPolicy {
        r#type: RetryType::Exponential as i32,
        interval: 800,
        max_interval: 60000,
        max_retry: 0,
        basis: 2.0,
    };

    let job_timeout = 1200000;
    let total = policy.calculate_total_timeout_ms(job_timeout);

    assert_eq!(total, job_timeout);
}

#[test]
fn test_constant_retry_calculation() {
    let policy = RetryPolicy {
        r#type: RetryType::Constant as i32,
        interval: 1000, // 1 second
        max_interval: 60000,
        max_retry: 2,
        basis: 2.0,
    };

    let job_timeout = 10000; // 10 seconds

    // Expected: 10s × 3 executions + 1s × 2 intervals = 32s
    let total = policy.calculate_total_timeout_ms(job_timeout);

    assert_eq!(total, 32000);
}

#[test]
fn test_linear_retry_calculation() {
    let policy = RetryPolicy {
        r#type: RetryType::Linear as i32,
        interval: 1000, // 1 second
        max_interval: 60000,
        max_retry: 3,
        basis: 2.0,
    };

    let job_timeout = 10000; // 10 seconds

    // Intervals: 1s×1 + 1s×2 + 1s×3 = 6s
    // Expected: 10s × 4 executions + 6s = 46s
    let total = policy.calculate_total_timeout_ms(job_timeout);

    assert_eq!(total, 46000);
}

#[test]
fn test_exponential_retry_calculation() {
    let policy = RetryPolicy {
        r#type: RetryType::Exponential as i32,
        interval: 800,
        max_interval: 60000,
        max_retry: 1,
        basis: 2.0,
    };

    let job_timeout = 1200000; // 20 minutes

    // Intervals: 800ms × 2^0 = 800ms
    // Expected: 20min × 2 executions + 800ms = 2400800ms
    let total = policy.calculate_total_timeout_ms(job_timeout);

    assert_eq!(total, 2400800);
}

#[test]
fn test_exponential_retry_with_max_interval_cap() {
    let policy = RetryPolicy {
        r#type: RetryType::Exponential as i32,
        interval: 10000,     // 10 seconds
        max_interval: 15000, // 15 seconds cap
        max_retry: 3,
        basis: 2.0,
    };

    let job_timeout = 10000; // 10 seconds

    // Intervals without cap: 10s×2^0=10s, 10s×2^1=20s, 10s×2^2=40s
    // Intervals with cap: 10s, 15s (capped), 15s (capped) = 40s
    // Expected: 10s × 4 executions + 40s = 80s
    let total = policy.calculate_total_timeout_ms(job_timeout);

    assert_eq!(total, 80000);
}

#[test]
fn test_real_world_scenario() {
    // Based on the actual issue: retry_policy with max_retry=1, interval=800, exponential
    let policy = RetryPolicy {
        r#type: RetryType::Exponential as i32,
        interval: 800,
        max_interval: 60000,
        max_retry: 1,
        basis: 2.0,
    };

    let job_timeout = 1200000; // 20 minutes in ms

    let total = policy.calculate_total_timeout_ms(job_timeout);

    // Expected: 20min × 2 + 800ms = 40min + 800ms = 2400800ms
    assert_eq!(total, 2400800);
    assert!(total > 2 * job_timeout); // Must be greater than 2x job timeout
}

#[test]
fn test_calculate_direct_response_timeout_with_none() {
    let job_timeout = 1200000;
    let total = calculate_direct_response_timeout_ms(job_timeout, None);

    assert_eq!(total, Some(job_timeout));
}

#[test]
fn test_calculate_direct_response_timeout_with_policy() {
    let policy = RetryPolicy {
        r#type: RetryType::Constant as i32,
        interval: 1000,
        max_interval: 60000,
        max_retry: 1,
        basis: 2.0,
    };

    let job_timeout = 10000;
    let total = calculate_direct_response_timeout_ms(job_timeout, Some(&policy));

    // Expected: 10s × 2 + 1s = 21s
    assert_eq!(total, Some(21000));
}

#[test]
fn test_calculate_direct_response_timeout_zero_means_unlimited() {
    // timeout=0 means unlimited, should return None
    let job_timeout = 0;
    let total = calculate_direct_response_timeout_ms(job_timeout, None);
    assert_eq!(total, None);

    // Even with retry policy, timeout=0 should return None
    let policy = RetryPolicy {
        r#type: RetryType::Exponential as i32,
        interval: 1000,
        max_interval: 60000,
        max_retry: 3,
        basis: 2.0,
    };
    let total_with_policy = calculate_direct_response_timeout_ms(job_timeout, Some(&policy));
    assert_eq!(total_with_policy, None);
}
