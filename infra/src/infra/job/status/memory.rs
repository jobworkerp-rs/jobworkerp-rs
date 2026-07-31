use super::{JobProcessingStatusRecord, JobProcessingStatusRepository, StatusTransitionResult};
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
        let retried = self
            .atomic_hash_map
            .get(&id.value)
            .map(|record| record.retried)
            .unwrap_or(0);
        let res = self.atomic_hash_map.insert(
            id.value,
            JobProcessingStatusRecord {
                status: *status,
                retried,
            },
        );
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
                if v.status == JobProcessingStatus::Pending {
                    (JobId { value: *id }, JobProcessingStatus::Pending)
                } else if v.status == JobProcessingStatus::Running {
                    (JobId { value: *id }, JobProcessingStatus::Running)
                } else if v.status == JobProcessingStatus::WaitResult {
                    (JobId { value: *id }, JobProcessingStatus::WaitResult)
                } else if v.status == JobProcessingStatus::Cancelling {
                    (JobId { value: *id }, JobProcessingStatus::Cancelling)
                } else {
                    tracing::warn!(
                        "unknown status: id: {id}, status :{:?}. returning as Unknown",
                        v.status
                    );
                    (JobId { value: *id }, JobProcessingStatus::Unknown)
                }
            })
            .collect_vec())
    }
    async fn find_status(&self, id: &JobId) -> Result<Option<JobProcessingStatus>> {
        let res = self.atomic_hash_map.get(&id.value).map(|v| v.status);
        if let Some(v) = res {
            if v == JobProcessingStatus::Pending {
                Ok(Some(JobProcessingStatus::Pending))
            } else if v == JobProcessingStatus::Running {
                Ok(Some(JobProcessingStatus::Running))
            } else if v == JobProcessingStatus::WaitResult {
                Ok(Some(JobProcessingStatus::WaitResult))
            } else if v == JobProcessingStatus::Cancelling {
                Ok(Some(JobProcessingStatus::Cancelling))
            } else {
                tracing::warn!(
                    "unknown status in memory: id: {}, status :{:?}. returning as Unknown",
                    &id.value,
                    v
                );
                Ok(Some(JobProcessingStatus::Unknown))
            }
        } else {
            Ok(None)
        }
    }

    async fn find_status_record(&self, id: &JobId) -> Result<Option<JobProcessingStatusRecord>> {
        Ok(self.atomic_hash_map.get(&id.value).map(|value| *value))
    }

    async fn compare_and_set_status(
        &self,
        id: &JobId,
        expected: Option<JobProcessingStatusRecord>,
        next: Option<JobProcessingStatusRecord>,
    ) -> Result<StatusTransitionResult> {
        use dashmap::mapref::entry::Entry;

        match self.atomic_hash_map.entry(id.value) {
            Entry::Occupied(mut entry) => {
                let current = *entry.get();
                if Some(current) != expected {
                    return Ok(StatusTransitionResult::Conflict(Some(current)));
                }
                match next {
                    Some(next) => {
                        entry.insert(next);
                    }
                    None => {
                        entry.remove();
                    }
                }
                Ok(StatusTransitionResult::Applied)
            }
            Entry::Vacant(entry) => {
                if expected.is_some() {
                    return Ok(StatusTransitionResult::Conflict(None));
                }
                if let Some(next) = next {
                    entry.insert(next);
                }
                Ok(StatusTransitionResult::Applied)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryJobProcessingStatusRepository {
    atomic_hash_map: Arc<DashMap<i64, JobProcessingStatusRecord>>,
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
        assert!(
            repo.upsert_status(&id, &JobProcessingStatus::Running)
                .await
                .unwrap(),
        );
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
        repo.atomic_hash_map.insert(
            id.value,
            JobProcessingStatusRecord {
                status: JobProcessingStatus::Unknown,
                retried: 0,
            },
        );

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

    #[tokio::test]
    async fn compare_and_set_restores_only_the_claimed_attempt_and_preserves_cancelling() {
        let repo = MemoryJobProcessingStatusRepository::new();
        let id = JobId { value: 2 };
        let pending = JobProcessingStatusRecord {
            status: JobProcessingStatus::Pending,
            retried: 1,
        };
        let running = JobProcessingStatusRecord {
            status: JobProcessingStatus::Running,
            retried: 1,
        };
        let cancelling = JobProcessingStatusRecord {
            status: JobProcessingStatus::Cancelling,
            retried: 1,
        };

        assert_eq!(
            repo.compare_and_set_status(&id, None, Some(pending))
                .await
                .unwrap(),
            StatusTransitionResult::Applied
        );
        assert_eq!(
            repo.compare_and_set_status(&id, Some(pending), Some(running))
                .await
                .unwrap(),
            StatusTransitionResult::Applied
        );
        assert_eq!(
            repo.compare_and_set_status(&id, Some(running), Some(pending))
                .await
                .unwrap(),
            StatusTransitionResult::Applied
        );
        assert_eq!(
            repo.compare_and_set_status(&id, Some(pending), Some(cancelling))
                .await
                .unwrap(),
            StatusTransitionResult::Applied
        );
        assert_eq!(
            repo.compare_and_set_status(&id, Some(running), Some(pending))
                .await
                .unwrap(),
            StatusTransitionResult::Conflict(Some(cancelling))
        );
    }
}
