use super::JobStatusRepository;
use anyhow::Result;
use dashmap::DashMap;
use itertools::Itertools;
use proto::jobworkerp::data::{JobId, JobStatus};
use std::sync::Arc;
use tonic::async_trait;

// manage job status (except for responseType:Direct worker)
// TODO use (listen after or create job status api)
#[async_trait]
impl JobStatusRepository for MemoryJobStatusRepository {
    async fn upsert_status(&self, id: &JobId, status: &JobStatus) -> Result<bool> {
        tracing::debug!("upsert_status to memory:{}={:?}", &id.value, status,);
        let res = self.atomic_hash_map.insert(id.value, *status as i32);
        Ok(res.is_some())
    }

    async fn delete_status(&self, id: &JobId) -> Result<bool> {
        tracing::debug!("delete_status from memory:{}", &id.value);
        Ok(self.atomic_hash_map.remove(&id.value).is_some())
    }

    async fn find_status_all(&self) -> Result<Vec<(JobId, JobStatus)>> {
        Ok(self
            .atomic_hash_map
            .iter()
            .flat_map(|r| {
                let (id, v) = r.pair();
                if *v == JobStatus::Pending as i32 {
                    Some((JobId { value: *id }, JobStatus::Pending))
                } else if *v == JobStatus::Running as i32 {
                    Some((JobId { value: *id }, JobStatus::Running))
                } else if *v == JobStatus::WaitResult as i32 {
                    Some((JobId { value: *id }, JobStatus::WaitResult))
                } else {
                    let msg = format!("unknown status: id: {id}, status :{v}.");
                    tracing::warn!(msg);
                    None
                }
            })
            .collect_vec())
    }
    async fn find_status(&self, id: &JobId) -> Result<Option<JobStatus>> {
        let res: Option<i32> = self.atomic_hash_map.get(&id.value).map(|v| *v);
        if let Some(v) = res {
            if v == JobStatus::Pending as i32 {
                Ok(Some(JobStatus::Pending))
            } else if v == JobStatus::Running as i32 {
                Ok(Some(JobStatus::Running))
            } else if v == JobStatus::WaitResult as i32 {
                Ok(Some(JobStatus::WaitResult))
            } else {
                tracing::warn!(
                    "unknown status in memory: id: {}, status :{}. delete",
                    &id.value,
                    v
                );
                self.delete_status(id).await?;
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryJobStatusRepository {
    atomic_hash_map: Arc<DashMap<i64, i32>>,
}
impl MemoryJobStatusRepository {
    pub fn new() -> Self {
        Self {
            atomic_hash_map: Arc::new(DashMap::new()),
        }
    }
}

impl Default for MemoryJobStatusRepository {
    fn default() -> Self {
        Self::new()
    }
}

// create test for upsert_status, delete_status, find_status_all, find_status
#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::data::{JobId, JobStatus};

    #[tokio::test]
    async fn test_memory_job_status_repository() {
        let repo = MemoryJobStatusRepository::new();
        let id = JobId { value: 1 };
        let status = JobStatus::Pending;
        assert!(!repo.upsert_status(&id, &status).await.unwrap());
        assert_eq!(
            repo.find_status(&id).await.unwrap(),
            Some(JobStatus::Pending)
        );
        assert!(repo.upsert_status(&id, &JobStatus::Running).await.unwrap(),);
        assert_eq!(
            repo.find_status(&id).await.unwrap(),
            Some(JobStatus::Running)
        );
        assert!(repo.delete_status(&id).await.unwrap());
        assert_eq!(repo.find_status(&id).await.unwrap(), None);
        assert!(!repo.delete_status(&id).await.unwrap());
    }
}
