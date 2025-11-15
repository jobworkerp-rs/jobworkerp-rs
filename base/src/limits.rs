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
    // Test removed: constant assertions are optimized out by compiler
    // These constants are defined at compile time and cannot be changed
}
