use super::JobProcessingStatusRepository;
use anyhow::Result;
use dashmap::DashMap;
use itertools::Itertools;
use proto::jobworkerp::data::{JobId, JobProcessingStatus};
use std::sync::Arc;
use tonic::async_trait;

// manage job status (except for responseType:Direct worker)
// TODO use (listen after or create job status api)
#[async_trait]
impl JobProcessingStatusRepository for MemoryJobProcessingStatusRepository {
    async fn upsert_status(&self, id: &JobId, status: &JobProcessingStatus) -> Result<bool> {
        tracing::debug!("upsert_status to memory:{}={:?}", &id.value, status,);
        let res = self.atomic_hash_map.insert(id.value, *status as i32);
        Ok(res.is_some())
    }

    async fn delete_status(&self, id: &JobId) -> Result<bool> {
        tracing::debug!("delete_status from memory:{}", &id.value);
        Ok(self.atomic_hash_map.remove(&id.value).is_some())
    }

    async fn find_status_all(&self) -> Result<Vec<(JobId, JobProcessingStatus)>> {
        Ok(self
            .atomic_hash_map
            .iter()
            .map(|r| {
                let (id, v) = r.pair();
                if *v == JobProcessingStatus::Pending as i32 {
                    (JobId { value: *id }, JobProcessingStatus::Pending)
                } else if *v == JobProcessingStatus::Running as i32 {
                    (JobId { value: *id }, JobProcessingStatus::Running)
                } else if *v == JobProcessingStatus::WaitResult as i32 {
                    (JobId { value: *id }, JobProcessingStatus::WaitResult)
                } else {
                    tracing::warn!("unknown status: id: {id}, status :{v}. returning as Unknown");
                    (JobId { value: *id }, JobProcessingStatus::Unknown)
                }
            })
            .collect_vec())
    }
    async fn find_status(&self, id: &JobId) -> Result<Option<JobProcessingStatus>> {
        let res: Option<i32> = self.atomic_hash_map.get(&id.value).map(|v| *v);
        if let Some(v) = res {
            if v == JobProcessingStatus::Pending as i32 {
                Ok(Some(JobProcessingStatus::Pending))
            } else if v == JobProcessingStatus::Running as i32 {
                Ok(Some(JobProcessingStatus::Running))
            } else if v == JobProcessingStatus::WaitResult as i32 {
                Ok(Some(JobProcessingStatus::WaitResult))
            } else {
                tracing::warn!(
                    "unknown status in memory: id: {}, status :{}. returning as Unknown",
                    &id.value,
                    v
                );
                Ok(Some(JobProcessingStatus::Unknown))
            }
        } else {
            Ok(None)
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryJobProcessingStatusRepository {
    atomic_hash_map: Arc<DashMap<i64, i32>>,
}
impl MemoryJobProcessingStatusRepository {
    pub fn new() -> Self {
        Self {
            atomic_hash_map: Arc::new(DashMap::new()),
        }
    }
}

impl Default for MemoryJobProcessingStatusRepository {
    fn default() -> Self {
        Self::new()
    }
}

// create test for upsert_status, delete_status, find_status_all, find_status
#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::data::{JobId, JobProcessingStatus};

    #[tokio::test]
    async fn test_memory_job_status_repository() {
        let repo = MemoryJobProcessingStatusRepository::new();
        let id = JobId { value: 1 };
        let status = JobProcessingStatus::Pending;
        assert!(!repo.upsert_status(&id, &status).await.unwrap());
        assert_eq!(
            repo.find_status(&id).await.unwrap(),
            Some(JobProcessingStatus::Pending)
        );
        assert!(repo.upsert_status(&id, &JobProcessingStatus::Running).await.unwrap(),);
        assert_eq!(
            repo.find_status(&id).await.unwrap(),
            Some(JobProcessingStatus::Running)
        );
        assert!(repo.delete_status(&id).await.unwrap());
        assert_eq!(repo.find_status(&id).await.unwrap(), None);
        assert!(!repo.delete_status(&id).await.unwrap());
    }

    #[tokio::test]
    async fn test_memory_job_status_repository_unknown_status() {
        let repo = MemoryJobProcessingStatusRepository::new();
        let id = JobId { value: 1 };
        
        // Insert an invalid/unknown status value directly into the map
        repo.atomic_hash_map.insert(id.value, 999); // Invalid status value
        
        // Should return Unknown instead of None or error
        assert_eq!(
            repo.find_status(&id).await.unwrap(),
            Some(JobProcessingStatus::Unknown)
        );
        
        // find_status_all should also return Unknown for invalid statuses
        let all_statuses = repo.find_status_all().await.unwrap();
        assert_eq!(all_statuses.len(), 1);
        assert_eq!(all_statuses[0].1, JobProcessingStatus::Unknown);
    }
}
