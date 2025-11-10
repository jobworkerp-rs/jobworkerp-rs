/// Validation limits for admin UI gRPC services
///
/// These constants define the maximum values for various request parameters
/// to prevent DoS attacks and ensure system stability.
/// Maximum number of items to return in a single FindList request
pub const MAX_LIMIT: i32 = 1000;

/// Maximum offset value for pagination
pub const MAX_OFFSET: i64 = 10000;

/// Maximum number of IDs in filter parameters (worker_ids, runner_ids, etc.)
pub const MAX_FILTER_IDS: usize = 100;

/// Maximum length of channel name
pub const MAX_CHANNEL_NAME_LENGTH: usize = 255;

/// Maximum length of name filter string
pub const MAX_NAME_FILTER_LENGTH: usize = 255;

/// Maximum time range in days for time-based filters
pub const MAX_TIME_RANGE_DAYS: i64 = 365;

/// Maximum number of statuses/priorities in filter arrays
pub const MAX_FILTER_ENUMS: usize = 10;

/// Maximum number of records to delete in a single bulk operation
pub const MAX_BULK_DELETE: i32 = 100_000;

/// Maximum time range for filtering (1 year in milliseconds)
pub const MAX_TIME_RANGE_MS: i64 = 365 * 24 * 60 * 60 * 1000;

/// Minimum retention period for bulk deletion (1 day in milliseconds)
/// Safety check to prevent accidental deletion of recent data
pub const MIN_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limits_are_positive() {
        assert!(MAX_LIMIT > 0);
        assert!(MAX_OFFSET > 0);
        assert!(MAX_FILTER_IDS > 0);
        assert!(MAX_CHANNEL_NAME_LENGTH > 0);
        assert!(MAX_NAME_FILTER_LENGTH > 0);
        assert!(MAX_TIME_RANGE_DAYS > 0);
        assert!(MAX_FILTER_ENUMS > 0);
    }

    #[test]
    fn test_reasonable_limits() {
        // Ensure limits are reasonable for typical use cases
        assert!(
            MAX_LIMIT <= 10000,
            "MAX_LIMIT should be reasonable for memory consumption"
        );
        assert!(
            MAX_OFFSET <= 100000,
            "MAX_OFFSET should prevent excessive pagination"
        );
        assert!(
            MAX_FILTER_IDS <= 1000,
            "MAX_FILTER_IDS should prevent SQL query size explosion"
        );
    }
}
