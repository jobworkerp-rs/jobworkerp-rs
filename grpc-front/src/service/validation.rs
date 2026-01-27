// Validation functions for admin UI gRPC services
//
// These functions validate request parameters to prevent DoS attacks
// and ensure system stability by enforcing limits defined in base/src/limits.rs

use jobworkerp_base::limits::*;
use tonic::Status;

/// Validate limit parameter
///
/// # Arguments
/// * `limit` - Optional limit value
///
/// # Returns
/// * `Ok(())` if valid or None
/// * `Err(Status)` if limit exceeds maximum or is negative
pub fn validate_limit(limit: Option<i32>) -> Result<(), Status> {
    if let Some(l) = limit {
        if l < 0 {
            return Err(Status::invalid_argument("limit must be non-negative"));
        }
        if l > MAX_LIMIT {
            return Err(Status::invalid_argument(format!(
                "limit exceeds maximum allowed value of {}",
                MAX_LIMIT
            )));
        }
    }
    Ok(())
}

/// Validate offset parameter
///
/// # Arguments
/// * `offset` - Optional offset value
///
/// # Returns
/// * `Ok(())` if valid or None
/// * `Err(Status)` if offset exceeds maximum or is negative
pub fn validate_offset(offset: Option<i64>) -> Result<(), Status> {
    if let Some(o) = offset {
        if o < 0 {
            return Err(Status::invalid_argument("offset must be non-negative"));
        }
        if o > MAX_OFFSET {
            return Err(Status::invalid_argument(format!(
                "offset exceeds maximum allowed value of {}",
                MAX_OFFSET
            )));
        }
    }
    Ok(())
}

/// Validate name_filter parameter
///
/// # Arguments
/// * `name_filter` - Optional name filter string
///
/// # Returns
/// * `Ok(())` if valid or None
/// * `Err(Status)` if name_filter exceeds maximum length
pub fn validate_name_filter(name_filter: Option<&String>) -> Result<(), Status> {
    if let Some(name) = name_filter
        && name.len() > MAX_NAME_FILTER_LENGTH
    {
        return Err(Status::invalid_argument(format!(
            "name_filter exceeds maximum length of {} characters",
            MAX_NAME_FILTER_LENGTH
        )));
    }
    Ok(())
}

/// Validate channel parameter
///
/// # Arguments
/// * `channel` - Optional channel name string
///
/// # Returns
/// * `Ok(())` if valid or None
/// * `Err(Status)` if channel name exceeds maximum length
pub fn validate_channel(channel: Option<&String>) -> Result<(), Status> {
    if let Some(ch) = channel
        && ch.len() > MAX_CHANNEL_NAME_LENGTH
    {
        return Err(Status::invalid_argument(format!(
            "channel name exceeds maximum length of {} characters",
            MAX_CHANNEL_NAME_LENGTH
        )));
    }
    Ok(())
}

/// Validate array size for filter IDs (worker_ids, runner_ids, etc.)
///
/// # Arguments
/// * `ids` - Slice of ID values
/// * `param_name` - Parameter name for error message
///
/// # Returns
/// * `Ok(())` if array size is within limit
/// * `Err(Status)` if array size exceeds maximum
pub fn validate_filter_ids<T>(ids: &[T], param_name: &str) -> Result<(), Status> {
    if ids.len() > MAX_FILTER_IDS {
        return Err(Status::invalid_argument(format!(
            "{} exceeds maximum array size of {}",
            param_name, MAX_FILTER_IDS
        )));
    }
    Ok(())
}

/// Validate array size for enum filters (runner_types, priorities, statuses, etc.)
///
/// # Arguments
/// * `enums` - Slice of enum values
/// * `param_name` - Parameter name for error message
///
/// # Returns
/// * `Ok(())` if array size is within limit
/// * `Err(Status)` if array size exceeds maximum
pub fn validate_filter_enums<T>(enums: &[T], param_name: &str) -> Result<(), Status> {
    if enums.len() > MAX_FILTER_ENUMS {
        return Err(Status::invalid_argument(format!(
            "{} exceeds maximum array size of {}",
            param_name, MAX_FILTER_ENUMS
        )));
    }
    Ok(())
}

/// Validate time range (from/to)
///
/// # Arguments
/// * `time_from` - Optional start time (milliseconds)
/// * `time_to` - Optional end time (milliseconds)
/// * `param_name` - Parameter name prefix for error message (e.g., "start_time", "end_time")
///
/// # Returns
/// * `Ok(())` if time range is valid or not specified
/// * `Err(Status)` if time range exceeds maximum or is invalid
pub fn validate_time_range(
    time_from: Option<i64>,
    time_to: Option<i64>,
    param_name: &str,
) -> Result<(), Status> {
    if let (Some(from), Some(to)) = (time_from, time_to) {
        if from < 0 || to < 0 {
            return Err(Status::invalid_argument(format!(
                "{} values must be non-negative",
                param_name
            )));
        }
        if from > to {
            return Err(Status::invalid_argument(format!(
                "{}_from must not be greater than {}_to",
                param_name, param_name
            )));
        }
        let range = to - from;
        if range > MAX_TIME_RANGE_MS {
            return Err(Status::invalid_argument(format!(
                "{} range exceeds maximum allowed range of {} days",
                param_name, MAX_TIME_RANGE_DAYS
            )));
        }
    } else {
        if let Some(from) = time_from
            && from < 0
        {
            return Err(Status::invalid_argument(format!(
                "{}_from must be non-negative",
                param_name
            )));
        }
        if let Some(to) = time_to
            && to < 0
        {
            return Err(Status::invalid_argument(format!(
                "{}_to must be non-negative",
                param_name
            )));
        }
    }
    Ok(())
}

/// Validate bulk delete request safety
///
/// # Arguments
/// * `end_time_before` - Optional timestamp before which to delete
///
/// # Returns
/// * `Ok(())` if delete request is safe
/// * `Err(Status)` if deletion would affect recent data
pub fn validate_bulk_delete_safety(end_time_before: Option<i64>) -> Result<(), Status> {
    if let Some(before) = end_time_before {
        let now = command_utils::util::datetime::now_millis();
        let retention_boundary = now - MIN_RETENTION_MS;

        if before > retention_boundary {
            return Err(Status::invalid_argument(format!(
                "Cannot delete data newer than {} hours. \
                 Requested deletion up to {}, but minimum retention boundary is {}",
                MIN_RETENTION_MS / 1000 / 60 / 60,
                before,
                retention_boundary
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_limit_valid() {
        assert!(validate_limit(None).is_ok());
        assert!(validate_limit(Some(0)).is_ok());
        assert!(validate_limit(Some(100)).is_ok());
        assert!(validate_limit(Some(MAX_LIMIT)).is_ok());
    }

    #[test]
    fn test_validate_limit_invalid() {
        assert!(validate_limit(Some(-1)).is_err());
        assert!(validate_limit(Some(MAX_LIMIT + 1)).is_err());
    }

    #[test]
    fn test_validate_offset_valid() {
        assert!(validate_offset(None).is_ok());
        assert!(validate_offset(Some(0)).is_ok());
        assert!(validate_offset(Some(100)).is_ok());
        assert!(validate_offset(Some(MAX_OFFSET)).is_ok());
    }

    #[test]
    fn test_validate_offset_invalid() {
        assert!(validate_offset(Some(-1)).is_err());
        assert!(validate_offset(Some(MAX_OFFSET + 1)).is_err());
    }

    #[test]
    fn test_validate_name_filter_valid() {
        assert!(validate_name_filter(None).is_ok());
        assert!(validate_name_filter(Some(&String::from("test"))).is_ok());
        assert!(validate_name_filter(Some(&"a".repeat(MAX_NAME_FILTER_LENGTH))).is_ok());
    }

    #[test]
    fn test_validate_name_filter_invalid() {
        let too_long = "a".repeat(MAX_NAME_FILTER_LENGTH + 1);
        assert!(validate_name_filter(Some(&too_long)).is_err());
    }

    #[test]
    fn test_validate_filter_ids_valid() {
        let ids: Vec<i64> = vec![];
        assert!(validate_filter_ids(&ids, "test_ids").is_ok());

        let ids: Vec<i64> = vec![1, 2, 3];
        assert!(validate_filter_ids(&ids, "test_ids").is_ok());

        let ids: Vec<i64> = (0..MAX_FILTER_IDS).map(|i| i as i64).collect();
        assert!(validate_filter_ids(&ids, "test_ids").is_ok());
    }

    #[test]
    fn test_validate_filter_ids_invalid() {
        let ids: Vec<i64> = (0..MAX_FILTER_IDS + 1).map(|i| i as i64).collect();
        assert!(validate_filter_ids(&ids, "test_ids").is_err());
    }

    #[test]
    fn test_validate_filter_ids_error_message_includes_param_name() {
        let ids: Vec<i64> = (0..MAX_FILTER_IDS + 1).map(|i| i as i64).collect();
        let err = validate_filter_ids(&ids, "worker_ids").unwrap_err();
        assert!(
            err.message().contains("worker_ids"),
            "error message should include parameter name"
        );
    }

    #[test]
    fn test_validate_filter_enums_valid() {
        let enums: Vec<i32> = vec![];
        assert!(validate_filter_enums(&enums, "test_enums").is_ok());

        let enums: Vec<i32> = vec![1, 2, 3];
        assert!(validate_filter_enums(&enums, "test_enums").is_ok());

        let enums: Vec<i32> = (0..MAX_FILTER_ENUMS).map(|i| i as i32).collect();
        assert!(validate_filter_enums(&enums, "test_enums").is_ok());
    }

    #[test]
    fn test_validate_filter_enums_invalid() {
        let enums: Vec<i32> = (0..MAX_FILTER_ENUMS + 1).map(|i| i as i32).collect();
        assert!(validate_filter_enums(&enums, "test_enums").is_err());
    }

    #[test]
    fn test_validate_filter_enums_error_message_includes_param_name() {
        let enums: Vec<i32> = (0..MAX_FILTER_ENUMS + 1).map(|i| i as i32).collect();
        let err = validate_filter_enums(&enums, "statuses").unwrap_err();
        assert!(
            err.message().contains("statuses"),
            "error message should include parameter name"
        );
    }
    #[test]
    fn test_validate_time_range_valid() {
        assert!(validate_time_range(None, None, "test_time").is_ok());
        assert!(validate_time_range(Some(0), None, "test_time").is_ok());
        assert!(validate_time_range(None, Some(1000), "test_time").is_ok());
        assert!(validate_time_range(Some(0), Some(1000), "test_time").is_ok());

        // Maximum allowed range (1 year)
        assert!(validate_time_range(Some(0), Some(MAX_TIME_RANGE_MS), "test_time").is_ok());
    }

    #[test]
    fn test_validate_time_range_invalid() {
        // Negative values
        assert!(validate_time_range(Some(-1), Some(1000), "test_time").is_err());
        assert!(validate_time_range(Some(0), Some(-1), "test_time").is_err());

        // from > to
        assert!(validate_time_range(Some(1000), Some(500), "test_time").is_err());

        // Range exceeds maximum
        assert!(validate_time_range(Some(0), Some(MAX_TIME_RANGE_MS + 1), "test_time").is_err());
    }

    #[test]
    fn test_validate_channel_valid() {
        assert!(validate_channel(None).is_ok());
        assert!(validate_channel(Some(&String::from("test"))).is_ok());
        assert!(validate_channel(Some(&"a".repeat(MAX_CHANNEL_NAME_LENGTH))).is_ok());
    }

    #[test]
    fn test_validate_channel_invalid() {
        let too_long = "a".repeat(MAX_CHANNEL_NAME_LENGTH + 1);
        assert!(validate_channel(Some(&too_long)).is_err());
    }

    #[test]
    fn test_validate_bulk_delete_safety() {
        // Future timestamp should fail (within retention period)
        let now = command_utils::util::datetime::now_millis();
        let recent = now - (MIN_RETENTION_MS / 2); // 12 hours ago
        assert!(validate_bulk_delete_safety(Some(recent)).is_err());

        // Old timestamp should succeed (beyond retention period)
        let old = now - (MIN_RETENTION_MS * 2); // 48 hours ago
        assert!(validate_bulk_delete_safety(Some(old)).is_ok());

        // None should succeed (no time filter)
        assert!(validate_bulk_delete_safety(None).is_ok());
    }
}
