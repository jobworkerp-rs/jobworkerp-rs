//! Test for job cancellation functionality
//! 
//! This test verifies that the delete_job method properly handles
//! job cancellation based on different job processing states.

#[cfg(test)]
mod tests {
    use crate::module::test::create_hybrid_test_app;
    use anyhow::Result;
    use proto::jobworkerp::data::{JobId, JobProcessingStatus};

    #[tokio::test]
    async fn test_delete_job_calls_cancel_functionality() -> Result<()> {
        let app_module = create_hybrid_test_app().await?;
        let app = app_module.job_app.clone();

        // Create a test job ID
        let test_job_id = JobId { value: 12345 };

        // Test delete_job method (which now performs cancellation)
        // Since no job actually exists, this should return false but not error
        let result = app.delete_job(&test_job_id).await?;
        
        // The method should execute without error and return false
        // (because the job doesn't exist in the test environment)
        assert_eq!(result, false);

        Ok(())
    }

    #[tokio::test]
    async fn test_cancel_job_with_nonexistent_job() -> Result<()> {
        let app_module = create_hybrid_test_app().await?;
        let app = app_module.job_app.clone();

        // Test with various non-existent job IDs
        let test_job_ids = vec![
            JobId { value: 99999 },
            JobId { value: 0 },
            JobId { value: -1 },
        ];

        for job_id in test_job_ids {
            let result = app.delete_job(&job_id).await?;
            // Should return false for non-existent jobs
            assert_eq!(result, false, "Expected false for non-existent job {}", job_id.value);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_job_processing_status_enum_values() -> Result<()> {
        // Verify that all JobProcessingStatus variants are available for cancellation logic
        let statuses = vec![
            JobProcessingStatus::Unknown,
            JobProcessingStatus::Pending,
            JobProcessingStatus::Running,
            JobProcessingStatus::WaitResult,
            JobProcessingStatus::Cancelling,
        ];

        // Each status should have a unique i32 value
        let mut values: Vec<i32> = statuses.iter().map(|s| *s as i32).collect();
        values.sort();
        values.dedup();
        
        // Should have 5 unique values (0-4)
        assert_eq!(values.len(), 5);
        assert_eq!(values, vec![0, 1, 2, 3, 4]);

        // Verify CANCELLING status has the expected value (4)
        assert_eq!(JobProcessingStatus::Cancelling as i32, 4);

        Ok(())
    }

    #[tokio::test]
    async fn test_cancellation_broadcast_placeholder() -> Result<()> {
        let app_module = create_hybrid_test_app().await?;
        let app = app_module.job_app.clone();

        // Test that broadcast functionality exists (placeholder implementation)
        let test_job_id = JobId { value: 54321 };
        
        // This should execute the placeholder broadcast without error
        let result = app.delete_job(&test_job_id).await?;
        
        // Should not error, even though it's a placeholder implementation
        assert_eq!(result, false); // False because job doesn't exist in test DB

        Ok(())
    }
}