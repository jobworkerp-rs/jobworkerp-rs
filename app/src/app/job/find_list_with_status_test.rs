#[cfg(test)]
mod tests {
    use super::super::JobApp;
    use crate::module::test::create_hybrid_test_app;
    use anyhow::Result;
    use proto::jobworkerp::data::JobStatus;

    #[tokio::test]
    async fn test_find_list_with_status_empty_result() -> Result<()> {
        let app_module = create_hybrid_test_app().await?;
        let app = app_module.job_app.clone();

        // When no jobs exist with the specified status, should return empty list
        let running_jobs = app
            .find_list_with_status(JobStatus::Running, Some(&10))
            .await?;
        assert_eq!(running_jobs.len(), 0);

        let pending_jobs = app
            .find_list_with_status(JobStatus::Pending, Some(&10))
            .await?;
        assert_eq!(pending_jobs.len(), 0);

        let wait_result_jobs = app
            .find_list_with_status(JobStatus::WaitResult, Some(&10))
            .await?;
        assert_eq!(wait_result_jobs.len(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_find_list_with_status_limit_zero() -> Result<()> {
        let app_module = create_hybrid_test_app().await?;
        let app = app_module.job_app.clone();

        // Test with limit 0 - should return empty list
        let jobs = app
            .find_list_with_status(JobStatus::Pending, Some(&0))
            .await?;
        assert_eq!(jobs.len(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_find_list_with_status_no_limit() -> Result<()> {
        let app_module = create_hybrid_test_app().await?;
        let app = app_module.job_app.clone();

        // Test with no limit (should use default of 100)
        let jobs = app.find_list_with_status(JobStatus::Pending, None).await?;
        // Should return empty list when no jobs exist
        assert_eq!(jobs.len(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_find_list_with_status_all_statuses() -> Result<()> {
        let app_module = create_hybrid_test_app().await?;
        let app = app_module.job_app.clone();

        // Test all job statuses are supported
        let statuses = vec![
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::WaitResult,
            JobStatus::Unknown,
        ];

        for status in statuses {
            let jobs = app.find_list_with_status(status, Some(&10)).await?;
            // Should not panic and should return empty list when no jobs exist
            assert_eq!(jobs.len(), 0);

            // Verify each returned job has the correct status (none in this case)
            for (job, returned_status) in jobs {
                assert_eq!(returned_status, status);
                assert!(job.id.is_some());
            }
        }

        Ok(())
    }
}
