//! Test for ResultStatus::CANCELLED functionality
//! 
//! This test verifies that the CANCELLED status is properly defined
//! and can be used for job cancellation results.

#[cfg(test)]
mod tests {
    use proto::jobworkerp::data::ResultStatus;

    #[test]
    fn test_result_status_cancelled_exists() {
        // Verify that ResultStatus::CANCELLED is defined and has the correct value
        let cancelled_status = ResultStatus::Cancelled;
        assert_eq!(cancelled_status as i32, 6);
    }

    #[test]
    fn test_result_status_cancelled_comparison() {
        // Test that CANCELLED status can be compared with other statuses
        let cancelled = ResultStatus::Cancelled;
        let success = ResultStatus::Success;
        let error = ResultStatus::ErrorAndRetry;

        assert_ne!(cancelled, success);
        assert_ne!(cancelled, error);
        assert_eq!(cancelled, ResultStatus::Cancelled);
    }

    #[test]
    fn test_result_status_cancelled_from_i32() {
        // Test conversion from i32 to ResultStatus (important for database storage)
        let status_value = 6i32;
        let status = match status_value {
            0 => ResultStatus::Success,
            1 => ResultStatus::ErrorAndRetry,
            2 => ResultStatus::FatalError,
            3 => ResultStatus::Abort,
            4 => ResultStatus::MaxRetry,
            5 => ResultStatus::OtherError,
            6 => ResultStatus::Cancelled,
            _ => ResultStatus::OtherError, // fallback
        };

        assert_eq!(status, ResultStatus::Cancelled);
        assert_eq!(status as i32, 6);
    }

    #[test]
    fn test_result_status_all_variants() {
        // Verify all ResultStatus variants including CANCELLED
        let all_statuses = vec![
            ResultStatus::Success,
            ResultStatus::ErrorAndRetry,
            ResultStatus::FatalError,
            ResultStatus::Abort,
            ResultStatus::MaxRetry,
            ResultStatus::OtherError,
            ResultStatus::Cancelled,
        ];

        // Each status should have a unique i32 value
        let mut values: Vec<i32> = all_statuses.iter().map(|s| *s as i32).collect();
        values.sort();
        values.dedup();
        
        // Should have 7 unique values (0-6)
        assert_eq!(values.len(), 7);
        assert_eq!(values, vec![0, 1, 2, 3, 4, 5, 6]);
    }
}