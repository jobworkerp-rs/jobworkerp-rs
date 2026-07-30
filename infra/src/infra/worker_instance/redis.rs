use anyhow::Result;
use async_trait::async_trait;
use infra_utils::infra::redis::RedisPool;
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::{WorkerInstance, WorkerInstanceData, WorkerInstanceId};
use redis::{AsyncCommands, cmd};
use std::collections::BTreeMap;
use std::io::Cursor;

use super::{ExpiredWorkerInstance, WorkerInstanceRecoveryRepository, WorkerInstanceRepository};

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
    const REGISTRY_KEY: &'static str = "WORKER_INSTANCE_REGISTRY:{worker-instance}";
    const HEARTBEAT_KEY: &'static str = "WORKER_INSTANCE_HEARTBEAT:{worker-instance}";

    fn recovery_lock_key(id: i64) -> String {
        format!("WORKER_INSTANCE_RECOVERY_LOCK:{{worker-instance}}:{id}")
    }

    pub fn new(redis_pool: &'static RedisPool) -> Self {
        Self { redis_pool }
    }

    fn serialize(instance: &WorkerInstance) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(instance.encoded_len());
        instance
            .encode(&mut buf)
            .map_err(|e| anyhow::anyhow!("encode error: {}", e))?;
        Ok(buf)
    }

    fn deserialize(buf: &[u8]) -> Result<WorkerInstance> {
        WorkerInstance::decode(&mut Cursor::new(buf))
            .map_err(|e| anyhow::anyhow!("decode error: {}", e))
    }

    async fn heartbeat_value(&self, id: i64) -> Result<Option<i64>> {
        Ok(self
            .redis_pool
            .get()
            .await?
            .hget(Self::HEARTBEAT_KEY, id)
            .await
            .map_err(JobWorkerError::RedisError)?)
    }

    async fn redis_now_millis(&self) -> Result<i64> {
        let mut connection = self.redis_pool.get().await?;
        let (seconds, microseconds): (i64, i64) = cmd("TIME")
            .query_async(&mut *connection)
            .await
            .map_err(JobWorkerError::RedisError)?;
        Ok(seconds * 1000 + microseconds / 1000)
    }

    fn with_heartbeat(mut instance: WorkerInstance, heartbeat: Option<i64>) -> WorkerInstance {
        if let (Some(data), Some(heartbeat)) = (instance.data.as_mut(), heartbeat) {
            data.last_heartbeat = heartbeat;
        }
        instance
    }
}

#[async_trait]
impl WorkerInstanceRepository for RedisWorkerInstanceRepository {
    async fn upsert(&self, id: &WorkerInstanceId, data: &WorkerInstanceData) -> Result<bool> {
        let instance = WorkerInstance {
            id: Some(*id),
            data: Some(data.clone()),
        };

        let script = r#"
            if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 1 then return 0 end
            if redis.call('EXISTS', KEYS[3]) == 1 then return -1 end
            local time = redis.call('TIME')
            local now = time[1] * 1000 + math.floor(time[2] / 1000)
            redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
            redis.call('HSET', KEYS[2], ARGV[1], now)
            return 1
        "#;
        let mut connection = self.redis_pool.get().await?;
        let result: i64 = cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(Self::REGISTRY_KEY)
            .arg(Self::HEARTBEAT_KEY)
            .arg(Self::recovery_lock_key(id.value))
            .arg(id.value)
            .arg(Self::serialize(&instance)?)
            .query_async(&mut *connection)
            .await
            .map_err(JobWorkerError::RedisError)?;
        if result == -1 {
            return Err(anyhow::anyhow!(
                "worker instance is protected by a recovery lock: {}",
                id.value
            ));
        }

        tracing::debug!(
            "upsert worker instance to redis: id={}, result={}",
            id.value,
            result
        );
        Ok(result == 0)
    }

    async fn update_heartbeat(&self, id: &WorkerInstanceId) -> Result<bool> {
        let script = r#"
            if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 then return 0 end
            if redis.call('HEXISTS', KEYS[2], ARGV[1]) == 0 then return 0 end
            if redis.call('EXISTS', KEYS[3]) == 1 then return -1 end
            local time = redis.call('TIME')
            redis.call('HSET', KEYS[2], ARGV[1], time[1] * 1000 + math.floor(time[2] / 1000))
            return 1
        "#;
        let mut connection = self.redis_pool.get().await?;
        let result: i64 = cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(Self::REGISTRY_KEY)
            .arg(Self::HEARTBEAT_KEY)
            .arg(Self::recovery_lock_key(id.value))
            .arg(id.value)
            .query_async(&mut *connection)
            .await
            .map_err(JobWorkerError::RedisError)?;
        Ok(result == 1)
    }

    async fn delete(&self, id: &WorkerInstanceId) -> Result<bool> {
        let deleted: i32 = self
            .redis_pool
            .get()
            .await?
            .hdel(Self::REGISTRY_KEY, id.value)
            .await
            .map_err(JobWorkerError::RedisError)?;

        tracing::debug!(
            "delete worker instance from redis: id={}, deleted={}",
            id.value,
            deleted > 0
        );
        if deleted > 0 {
            let _: i32 = self
                .redis_pool
                .get()
                .await?
                .hdel(Self::HEARTBEAT_KEY, id.value)
                .await
                .map_err(JobWorkerError::RedisError)?;
        }
        Ok(deleted > 0)
    }

    async fn find(&self, id: &WorkerInstanceId) -> Result<Option<WorkerInstance>> {
        let data: Option<Vec<u8>> = self
            .redis_pool
            .get()
            .await?
            .hget(Self::REGISTRY_KEY, id.value)
            .await
            .map_err(JobWorkerError::RedisError)?;

        match data {
            Some(buf) => Ok(Some(Self::with_heartbeat(
                Self::deserialize(&buf)?,
                self.heartbeat_value(id.value).await?,
            ))),
            None => Ok(None),
        }
    }

    async fn find_all(&self) -> Result<Vec<WorkerInstance>> {
        let all: BTreeMap<i64, Vec<u8>> = self
            .redis_pool
            .get()
            .await?
            .hgetall(Self::REGISTRY_KEY)
            .await
            .map_err(JobWorkerError::RedisError)?;

        let mut instances = Vec::with_capacity(all.len());
        for (id, buf) in all {
            instances.push(Self::with_heartbeat(
                Self::deserialize(&buf)?,
                self.heartbeat_value(id).await?,
            ));
        }
        Ok(instances)
    }

    async fn find_all_active(&self, timeout_millis: i64) -> Result<Vec<WorkerInstance>> {
        let now = self.redis_now_millis().await?;
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
        let now = self.redis_now_millis().await?;
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
                .hdel(Self::REGISTRY_KEY, id)
                .await
                .map_err(JobWorkerError::RedisError)?;
            if result > 0 {
                let _: i32 = conn
                    .hdel(Self::HEARTBEAT_KEY, id)
                    .await
                    .map_err(JobWorkerError::RedisError)?;
                deleted += 1;
                tracing::info!("deleted expired worker instance: id={}", id);
            }
        }

        Ok(deleted)
    }
}

#[async_trait]
impl WorkerInstanceRecoveryRepository for RedisWorkerInstanceRepository {
    async fn find_expired_for_recovery(
        &self,
        timeout_millis: i64,
    ) -> Result<Vec<ExpiredWorkerInstance>> {
        let cutoff = self.redis_now_millis().await? - timeout_millis;
        let mut connection = self.redis_pool.get().await?;
        let registry: BTreeMap<i64, Vec<u8>> = connection
            .hgetall(Self::REGISTRY_KEY)
            .await
            .map_err(JobWorkerError::RedisError)?;
        let heartbeats: BTreeMap<i64, i64> = connection
            .hgetall(Self::HEARTBEAT_KEY)
            .await
            .map_err(JobWorkerError::RedisError)?;

        registry
            .into_iter()
            .filter_map(|(id, registry_value)| {
                let heartbeat = heartbeats.get(&id).copied()?;
                (heartbeat < cutoff).then_some((registry_value, heartbeat))
            })
            .map(|(registry_value, observed_heartbeat_millis)| {
                let instance = Self::with_heartbeat(
                    Self::deserialize(&registry_value)?,
                    Some(observed_heartbeat_millis),
                );
                Ok(ExpiredWorkerInstance {
                    instance,
                    observed_heartbeat_millis,
                    registry_value,
                })
            })
            .collect()
    }

    async fn try_lock_expired(
        &self,
        expired: &ExpiredWorkerInstance,
        timeout_millis: i64,
        recovery_id: &str,
        lock_ttl_millis: i64,
    ) -> Result<bool> {
        let instance_id = expired
            .instance
            .id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("expired worker instance has no ID"))?
            .value;
        let script = r#"
            local record = redis.call('HGET', KEYS[1], ARGV[1])
            local heartbeat = redis.call('HGET', KEYS[2], ARGV[1])
            if not record or not heartbeat then return 0 end
            if record ~= ARGV[2] or heartbeat ~= ARGV[3] then return 0 end
            local time = redis.call('TIME')
            local now = time[1] * 1000 + math.floor(time[2] / 1000)
            if now - tonumber(heartbeat) < tonumber(ARGV[4]) then return 0 end
            return redis.call('SET', KEYS[3], ARGV[5], 'PX', ARGV[6], 'NX') and 1 or 0
        "#;
        let mut connection = self.redis_pool.get().await?;
        let locked: i64 = cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(Self::REGISTRY_KEY)
            .arg(Self::HEARTBEAT_KEY)
            .arg(Self::recovery_lock_key(instance_id))
            .arg(instance_id)
            .arg(&expired.registry_value)
            .arg(expired.observed_heartbeat_millis)
            .arg(timeout_millis)
            .arg(recovery_id)
            .arg(lock_ttl_millis)
            .query_async(&mut *connection)
            .await
            .map_err(JobWorkerError::RedisError)?;
        Ok(locked == 1)
    }

    async fn refresh_recovery_lock(
        &self,
        instance_id: i64,
        recovery_id: &str,
        lock_ttl_millis: i64,
    ) -> Result<bool> {
        let script = r#"
            if redis.call('GET', KEYS[1]) ~= ARGV[1] then return 0 end
            return redis.call('PEXPIRE', KEYS[1], ARGV[2])
        "#;
        let mut connection = self.redis_pool.get().await?;
        let refreshed: i64 = cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(Self::recovery_lock_key(instance_id))
            .arg(recovery_id)
            .arg(lock_ttl_millis)
            .query_async(&mut *connection)
            .await
            .map_err(JobWorkerError::RedisError)?;
        Ok(refreshed == 1)
    }

    async fn release_recovery_lock(&self, instance_id: i64, recovery_id: &str) -> Result<bool> {
        let script = r#"
            if redis.call('GET', KEYS[1]) ~= ARGV[1] then return 0 end
            return redis.call('DEL', KEYS[1])
        "#;
        let mut connection = self.redis_pool.get().await?;
        let released: i64 = cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(Self::recovery_lock_key(instance_id))
            .arg(recovery_id)
            .query_async(&mut *connection)
            .await
            .map_err(JobWorkerError::RedisError)?;
        Ok(released == 1)
    }

    async fn delete_expired_owned(
        &self,
        expired: &ExpiredWorkerInstance,
        timeout_millis: i64,
        recovery_id: &str,
    ) -> Result<bool> {
        let instance_id = expired
            .instance
            .id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("expired worker instance has no ID"))?
            .value;
        let script = r#"
            if redis.call('GET', KEYS[3]) ~= ARGV[1] then return 0 end
            local record = redis.call('HGET', KEYS[1], ARGV[2])
            local heartbeat = redis.call('HGET', KEYS[2], ARGV[2])
            if not record or not heartbeat or record ~= ARGV[3] or heartbeat ~= ARGV[4] then return 0 end
            local time = redis.call('TIME')
            local now = time[1] * 1000 + math.floor(time[2] / 1000)
            if now - tonumber(heartbeat) < tonumber(ARGV[5]) then return 0 end
            redis.call('HDEL', KEYS[1], ARGV[2])
            redis.call('HDEL', KEYS[2], ARGV[2])
            redis.call('DEL', KEYS[3])
            return 1
        "#;
        let mut connection = self.redis_pool.get().await?;
        let deleted: i64 = cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(Self::REGISTRY_KEY)
            .arg(Self::HEARTBEAT_KEY)
            .arg(Self::recovery_lock_key(instance_id))
            .arg(recovery_id)
            .arg(instance_id)
            .arg(&expired.registry_value)
            .arg(expired.observed_heartbeat_millis)
            .arg(timeout_millis)
            .query_async(&mut *connection)
            .await
            .map_err(JobWorkerError::RedisError)?;
        Ok(deleted == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use command_utils::util::datetime;
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
            rdb_status_index_recovery_version: 0,
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

            // Repository contract returns false when a registry entry is created.
            let result = repo.upsert(&id, &data).await.unwrap();
            assert!(!result, "First upsert should create the registry entry");

            // Find should return the instance
            let found = repo.find(&id).await.unwrap();
            assert!(found.is_some());
            let inst = found.unwrap();
            assert_eq!(inst.id.unwrap().value, 100001);
            assert_eq!(inst.data.as_ref().unwrap().ip_address, "192.168.1.100");

            // A duplicate registration preserves the initial static record.
            let result2 = repo.upsert(&id, &data).await.unwrap();
            assert!(result2, "Second upsert should report an existing entry");

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
        async fn recovery_lock_requires_the_observed_expired_record() {
            let repo = setup_repo().await;
            cleanup_repo(&repo).await;
            let id = WorkerInstanceId { value: 100003 };
            let now = datetime::now_millis();
            repo.upsert(
                &id,
                &create_test_data("192.168.1.103", None, vec![("default", 1)], now, now),
            )
            .await
            .unwrap();

            let expired = repo.find_expired_for_recovery(0).await.unwrap();
            assert_eq!(expired.len(), 1);
            let observed = &expired[0];
            assert!(
                repo.try_lock_expired(observed, 0, "recovery-a", 30_000)
                    .await
                    .unwrap()
            );
            assert!(!repo.update_heartbeat(&id).await.unwrap());
            assert!(
                !repo
                    .release_recovery_lock(id.value, "different-recovery")
                    .await
                    .unwrap()
            );
            assert!(
                repo.delete_expired_owned(observed, 0, "recovery-a")
                    .await
                    .unwrap()
            );
            assert!(repo.find(&id).await.unwrap().is_none());
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

            let buf = RedisWorkerInstanceRepository::serialize(&instance).unwrap();
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
