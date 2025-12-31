use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::datetime;
use deadpool_redis::redis::AsyncCommands;
use infra_utils::infra::redis::RedisPool;
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::{WorkerInstance, WorkerInstanceData, WorkerInstanceId};
use std::collections::BTreeMap;
use std::io::Cursor;

use super::WorkerInstanceRepository;

/// Redis-based implementation for Scalable configuration
///
/// # Behavior in Scalable Configuration
/// - Heartbeat updates `last_heartbeat` for active state tracking
/// - `find_all_active()` filters instances by heartbeat timeout
/// - `delete_expired()` removes instances with old heartbeats
/// - Handles crashed worker detection via heartbeat timeout
#[derive(Clone, Debug)]
pub struct RedisWorkerInstanceRepository {
    redis_pool: &'static RedisPool,
}

impl RedisWorkerInstanceRepository {
    const HASH_KEY: &'static str = "WORKER_INSTANCE";

    pub fn new(redis_pool: &'static RedisPool) -> Self {
        Self { redis_pool }
    }

    fn serialize(instance: &WorkerInstance) -> Vec<u8> {
        let mut buf = Vec::with_capacity(instance.encoded_len());
        instance.encode(&mut buf).expect("encode should not fail");
        buf
    }

    fn deserialize(buf: &[u8]) -> Result<WorkerInstance> {
        WorkerInstance::decode(&mut Cursor::new(buf))
            .map_err(|e| anyhow::anyhow!("decode error: {}", e))
    }
}

#[async_trait]
impl WorkerInstanceRepository for RedisWorkerInstanceRepository {
    async fn upsert(&self, id: &WorkerInstanceId, data: &WorkerInstanceData) -> Result<bool> {
        let instance = WorkerInstance {
            id: Some(*id),
            data: Some(data.clone()),
        };

        let result: bool = self
            .redis_pool
            .get()
            .await?
            .hset(Self::HASH_KEY, id.value, Self::serialize(&instance))
            .await
            .map_err(JobWorkerError::RedisError)?;

        tracing::debug!(
            "upsert worker instance to redis: id={}, result={}",
            id.value,
            result
        );
        Ok(result)
    }

    async fn update_heartbeat(&self, id: &WorkerInstanceId) -> Result<bool> {
        let mut conn = self.redis_pool.get().await?;

        let data: Option<Vec<u8>> = conn
            .hget(Self::HASH_KEY, id.value)
            .await
            .map_err(JobWorkerError::RedisError)?;

        match data {
            Some(buf) => {
                let mut instance = Self::deserialize(&buf)?;
                if let Some(ref mut d) = instance.data {
                    d.last_heartbeat = datetime::now_millis();
                }

                let _: bool = conn
                    .hset(Self::HASH_KEY, id.value, Self::serialize(&instance))
                    .await
                    .map_err(JobWorkerError::RedisError)?;

                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn delete(&self, id: &WorkerInstanceId) -> Result<bool> {
        let deleted: i32 = self
            .redis_pool
            .get()
            .await?
            .hdel(Self::HASH_KEY, id.value)
            .await
            .map_err(JobWorkerError::RedisError)?;

        tracing::debug!(
            "delete worker instance from redis: id={}, deleted={}",
            id.value,
            deleted > 0
        );
        Ok(deleted > 0)
    }

    async fn find(&self, id: &WorkerInstanceId) -> Result<Option<WorkerInstance>> {
        let data: Option<Vec<u8>> = self
            .redis_pool
            .get()
            .await?
            .hget(Self::HASH_KEY, id.value)
            .await
            .map_err(JobWorkerError::RedisError)?;

        match data {
            Some(buf) => Self::deserialize(&buf).map(Some),
            None => Ok(None),
        }
    }

    async fn find_all(&self) -> Result<Vec<WorkerInstance>> {
        let all: BTreeMap<i64, Vec<u8>> = self
            .redis_pool
            .get()
            .await?
            .hgetall(Self::HASH_KEY)
            .await
            .map_err(JobWorkerError::RedisError)?;

        all.values().map(|buf| Self::deserialize(buf)).collect()
    }

    async fn find_all_active(&self, timeout_millis: i64) -> Result<Vec<WorkerInstance>> {
        let now = datetime::now_millis();
        let cutoff = now - timeout_millis;

        let all = self.find_all().await?;

        Ok(all
            .into_iter()
            .filter(|inst| {
                inst.data
                    .as_ref()
                    .map(|d| d.last_heartbeat >= cutoff)
                    .unwrap_or(false)
            })
            .collect())
    }

    async fn delete_expired(&self, timeout_millis: i64) -> Result<u32> {
        let now = datetime::now_millis();
        let cutoff = now - timeout_millis;

        let all = self.find_all().await?;
        let expired_ids: Vec<i64> = all
            .iter()
            .filter(|inst| {
                inst.data
                    .as_ref()
                    .map(|d| d.last_heartbeat < cutoff)
                    .unwrap_or(true)
            })
            .filter_map(|inst| inst.id.as_ref().map(|id| id.value))
            .collect();

        if expired_ids.is_empty() {
            return Ok(0);
        }

        let mut conn = self.redis_pool.get().await?;
        let mut deleted = 0u32;

        for id in expired_ids {
            let result: i32 = conn
                .hdel(Self::HASH_KEY, id)
                .await
                .map_err(JobWorkerError::RedisError)?;
            if result > 0 {
                deleted += 1;
                tracing::info!("deleted expired worker instance: id={}", id);
            }
        }

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::data::ChannelConfig;

    fn create_test_data(
        ip: &str,
        hostname: Option<&str>,
        channels: Vec<(&str, u32)>,
        registered_at: i64,
        last_heartbeat: i64,
    ) -> WorkerInstanceData {
        WorkerInstanceData {
            ip_address: ip.to_string(),
            hostname: hostname.map(String::from),
            channels: channels
                .into_iter()
                .map(|(name, concurrency)| ChannelConfig {
                    name: name.to_string(),
                    concurrency,
                })
                .collect(),
            registered_at,
            last_heartbeat,
        }
    }

    #[cfg(feature = "test-utils")]
    mod redis_integration_tests {
        use super::*;

        async fn setup_repo() -> RedisWorkerInstanceRepository {
            let pool = infra_utils::infra::test::setup_test_redis_pool().await;
            RedisWorkerInstanceRepository::new(pool)
        }

        async fn cleanup_repo(repo: &RedisWorkerInstanceRepository) {
            // Clean up all test data
            let all = repo.find_all().await.unwrap();
            for inst in all {
                if let Some(id) = inst.id {
                    let _ = repo.delete(&id).await;
                }
            }
        }

        #[tokio::test]
        async fn test_upsert_and_find() {
            let repo = setup_repo().await;
            cleanup_repo(&repo).await;

            let id = WorkerInstanceId { value: 100001 };
            let now = datetime::now_millis();
            let data = create_test_data(
                "192.168.1.100",
                Some("test-host"),
                vec![("default", 4)],
                now,
                now,
            );

            // First upsert should return true (new field added)
            // Redis HSET returns 1 (true) for new field, 0 (false) for update
            let result = repo.upsert(&id, &data).await.unwrap();
            assert!(result, "First upsert should return true (new field)");

            // Find should return the instance
            let found = repo.find(&id).await.unwrap();
            assert!(found.is_some());
            let inst = found.unwrap();
            assert_eq!(inst.id.unwrap().value, 100001);
            assert_eq!(inst.data.as_ref().unwrap().ip_address, "192.168.1.100");

            // Second upsert should return false (update existing)
            let result2 = repo.upsert(&id, &data).await.unwrap();
            assert!(!result2, "Second upsert should return false (update)");

            cleanup_repo(&repo).await;
        }

        #[tokio::test]
        async fn test_update_heartbeat() {
            let repo = setup_repo().await;
            cleanup_repo(&repo).await;

            let id = WorkerInstanceId { value: 100002 };
            let old_time = datetime::now_millis() - 10000;
            let data = create_test_data(
                "192.168.1.101",
                None,
                vec![("default", 2)],
                old_time,
                old_time,
            );

            repo.upsert(&id, &data).await.unwrap();

            // Update heartbeat
            let updated = repo.update_heartbeat(&id).await.unwrap();
            assert!(updated);

            // Verify heartbeat was updated
            let found = repo.find(&id).await.unwrap().unwrap();
            let new_heartbeat = found.data.as_ref().unwrap().last_heartbeat;
            assert!(new_heartbeat > old_time);

            cleanup_repo(&repo).await;
        }

        #[tokio::test]
        async fn test_update_heartbeat_not_found() {
            let repo = setup_repo().await;

            let id = WorkerInstanceId { value: 999999 };
            let updated = repo.update_heartbeat(&id).await.unwrap();
            assert!(!updated);
        }

        #[tokio::test]
        async fn test_delete() {
            let repo = setup_repo().await;
            cleanup_repo(&repo).await;

            let id = WorkerInstanceId { value: 100003 };
            let now = datetime::now_millis();
            let data = create_test_data("192.168.1.102", None, vec![], now, now);

            repo.upsert(&id, &data).await.unwrap();

            // Delete should return true
            let deleted = repo.delete(&id).await.unwrap();
            assert!(deleted);

            // Find should return None
            let found = repo.find(&id).await.unwrap();
            assert!(found.is_none());

            // Delete again should return false
            let deleted_again = repo.delete(&id).await.unwrap();
            assert!(!deleted_again);
        }

        #[tokio::test]
        async fn test_find_all() {
            let repo = setup_repo().await;
            cleanup_repo(&repo).await;

            let now = datetime::now_millis();

            // Insert multiple instances
            for i in 1..=3 {
                let id = WorkerInstanceId { value: 100010 + i };
                let data = create_test_data(
                    &format!("192.168.1.{}", 100 + i),
                    Some(&format!("host-{}", i)),
                    vec![("default", 4)],
                    now,
                    now,
                );
                repo.upsert(&id, &data).await.unwrap();
            }

            let all = repo.find_all().await.unwrap();
            assert_eq!(all.len(), 3);

            cleanup_repo(&repo).await;
        }

        #[tokio::test]
        async fn test_find_all_active_timeout() {
            let repo = setup_repo().await;
            cleanup_repo(&repo).await;

            let now = datetime::now_millis();
            let timeout_millis: i64 = 5000; // 5 seconds

            // Active instance (recent heartbeat)
            let active_id = WorkerInstanceId { value: 100020 };
            let active_data = create_test_data("192.168.1.200", None, vec![], now, now);
            repo.upsert(&active_id, &active_data).await.unwrap();

            // Expired instance (old heartbeat)
            let expired_id = WorkerInstanceId { value: 100021 };
            let expired_data = create_test_data(
                "192.168.1.201",
                None,
                vec![],
                now - 10000,
                now - 10000, // 10 seconds ago
            );
            repo.upsert(&expired_id, &expired_data).await.unwrap();

            // Find active only
            let active = repo.find_all_active(timeout_millis).await.unwrap();
            assert_eq!(active.len(), 1);
            assert_eq!(active[0].id.as_ref().unwrap().value, 100020);

            cleanup_repo(&repo).await;
        }

        #[tokio::test]
        async fn test_delete_expired() {
            let repo = setup_repo().await;
            cleanup_repo(&repo).await;

            let now = datetime::now_millis();
            let timeout_millis: i64 = 5000;

            // Active instance
            let active_id = WorkerInstanceId { value: 100030 };
            let active_data = create_test_data("192.168.1.210", None, vec![], now, now);
            repo.upsert(&active_id, &active_data).await.unwrap();

            // Expired instances
            for i in 1..=2 {
                let id = WorkerInstanceId { value: 100030 + i };
                let data = create_test_data(
                    &format!("192.168.1.{}", 210 + i),
                    None,
                    vec![],
                    now - 10000,
                    now - 10000,
                );
                repo.upsert(&id, &data).await.unwrap();
            }

            // Before delete
            let all_before = repo.find_all().await.unwrap();
            assert_eq!(all_before.len(), 3);

            // Delete expired
            let deleted = repo.delete_expired(timeout_millis).await.unwrap();
            assert_eq!(deleted, 2);

            // After delete
            let all_after = repo.find_all().await.unwrap();
            assert_eq!(all_after.len(), 1);
            assert_eq!(all_after[0].id.as_ref().unwrap().value, 100030);

            cleanup_repo(&repo).await;
        }

        #[tokio::test]
        async fn test_serialize_deserialize() {
            let now = datetime::now_millis();
            let instance = WorkerInstance {
                id: Some(WorkerInstanceId { value: 12345 }),
                data: Some(create_test_data(
                    "10.0.0.1",
                    Some("test"),
                    vec![("ch1", 2), ("ch2", 4)],
                    now,
                    now,
                )),
            };

            let buf = RedisWorkerInstanceRepository::serialize(&instance);
            let decoded = RedisWorkerInstanceRepository::deserialize(&buf).unwrap();

            assert_eq!(decoded.id.unwrap().value, 12345);
            let data = decoded.data.unwrap();
            assert_eq!(data.ip_address, "10.0.0.1");
            assert_eq!(data.hostname, Some("test".to_string()));
            assert_eq!(data.channels.len(), 2);
        }

        #[tokio::test]
        async fn test_get_channel_aggregation() {
            let repo = setup_repo().await;
            cleanup_repo(&repo).await;

            let now = datetime::now_millis();

            // Add two instances with overlapping channels
            let id1 = WorkerInstanceId { value: 100040 };
            let data1 = create_test_data(
                "192.168.1.40",
                Some("host-40"),
                vec![("", 4), ("priority", 2)],
                now,
                now,
            );
            repo.upsert(&id1, &data1).await.unwrap();

            let id2 = WorkerInstanceId { value: 100041 };
            let data2 = create_test_data("192.168.1.41", Some("host-41"), vec![("", 8)], now, now);
            repo.upsert(&id2, &data2).await.unwrap();

            // Get aggregation
            let agg = repo.get_channel_aggregation(90000).await.unwrap();

            // Check default channel
            let default_agg = agg.get("").unwrap();
            assert_eq!(default_agg.total_concurrency, 4 + 8);
            assert_eq!(default_agg.active_instances, 2);

            // Check priority channel
            let priority_agg = agg.get("priority").unwrap();
            assert_eq!(priority_agg.total_concurrency, 2);
            assert_eq!(priority_agg.active_instances, 1);

            cleanup_repo(&repo).await;
        }
    }
}
