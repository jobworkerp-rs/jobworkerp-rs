use crate::app::JobBuilder;
use crate::module::AppModule;
use anyhow::Result;
use infra::infra::IdGeneratorWrapper;
use infra::infra::job::rdb::RdbJobRepository;
use infra::infra::job::status::execution::RdbJobStatusExecutionRepository;
use infra::infra::worker_instance::{ExpiredWorkerInstance, WorkerInstanceRecoveryRepository};
use jobworkerp_base::WORKER_INSTANCE_CONFIG;
use proto::jobworkerp::data::{JobResultData, JobResultId, QueueType, ResultOutput, ResultStatus};
use rand::RngExt;
use std::sync::Arc;

/// Best-effort recovery of RUNNING status rows belonging to a lost instance.
///
/// The RDB status row is claimed before publishing the failure.  A process
/// crash after that claim remains an intentional best-effort limitation; the
/// row is logical-deleted so a later execution cannot mistake it for RUNNING.
#[derive(Debug)]
pub struct WorkerInstanceRecoveryCoordinator {
    registry: Arc<dyn WorkerInstanceRecoveryRepository>,
    status: RdbJobStatusExecutionRepository,
    app_module: Arc<AppModule>,
    id_generator: IdGeneratorWrapper,
}

impl WorkerInstanceRecoveryCoordinator {
    pub fn new(
        registry: Arc<dyn WorkerInstanceRecoveryRepository>,
        status: RdbJobStatusExecutionRepository,
        app_module: Arc<AppModule>,
    ) -> Self {
        Self {
            registry,
            status,
            app_module,
            id_generator: IdGeneratorWrapper::new(),
        }
    }

    pub async fn recover_expired_instances(&self, timeout_millis: i64) -> Result<()> {
        for expired in self
            .registry
            .find_expired_for_recovery(timeout_millis)
            .await?
        {
            self.recover_instance(expired, timeout_millis).await?;
        }
        Ok(())
    }

    async fn recover_instance(
        &self,
        expired: ExpiredWorkerInstance,
        timeout_millis: i64,
    ) -> Result<()> {
        let instance_id = expired
            .instance
            .id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("expired worker instance has no ID"))?
            .value;
        let recovery_id = format!("{:016x}", rand::rng().random::<u64>());
        let config = &WORKER_INSTANCE_CONFIG.rdb_status_recovery;
        let lock_ttl_millis = i64::try_from(config.lock_ttl_sec.saturating_mul(1000))?;
        if !self
            .registry
            .try_lock_expired(&expired, timeout_millis, &recovery_id, lock_ttl_millis)
            .await?
        {
            return Ok(());
        }

        let outcome = self
            .recover_locked(&expired, &recovery_id, lock_ttl_millis)
            .await;
        match outcome {
            Ok(()) => {
                if !self
                    .registry
                    .delete_expired_owned(&expired, timeout_millis, &recovery_id)
                    .await?
                {
                    tracing::warn!(
                        instance_id,
                        "lost recovery ownership before expired instance deletion"
                    );
                }
            }
            Err(error) => {
                let _ = self
                    .registry
                    .release_recovery_lock(instance_id, &recovery_id)
                    .await;
                return Err(error);
            }
        }
        Ok(())
    }

    async fn recover_locked(
        &self,
        expired: &ExpiredWorkerInstance,
        recovery_id: &str,
        lock_ttl_millis: i64,
    ) -> Result<()> {
        let instance_id = expired.instance.id.as_ref().expect("validated ID").value;
        let candidates = self
            .status
            .find_running_by_instance(instance_id, 0, 1_000)
            .await?;
        for candidate in candidates {
            if !self
                .registry
                .refresh_recovery_lock(instance_id, recovery_id, lock_ttl_millis)
                .await?
            {
                anyhow::bail!("lost recovery lock for worker instance {instance_id}");
            }

            // Resolve before claiming so unavailable job/worker metadata leaves
            // the RUNNING row visible for a later recovery attempt.
            let job_id = RdbJobStatusExecutionRepository::candidate_job_id(&candidate);
            let Some(job) = self.app_module.job_app.find_job(&job_id).await? else {
                tracing::warn!(
                    job_id = job_id.value,
                    "lost job is absent; leaving status for retention cleanup"
                );
                continue;
            };
            let metadata = job.metadata;
            let Some(job_data) = job.data else {
                tracing::warn!(
                    job_id = job_id.value,
                    "lost job has no data; leaving status for later recovery"
                );
                continue;
            };
            let worker_id = RdbJobStatusExecutionRepository::candidate_worker_id(&candidate);
            let Some(worker_data) = self.app_module.worker_app.find_data(&worker_id).await? else {
                tracing::warn!(
                    worker_id = worker_id.value,
                    "lost job worker is absent; leaving status for later recovery"
                );
                continue;
            };

            let Some(claim) = self.status.claim_running(&candidate).await? else {
                continue;
            };
            let resolved =
                crate::app::job::resolve_job_params(&worker_data, job_data.overrides.as_ref());
            let now = command_utils::util::datetime::now_millis();
            let result_data = JobResultData {
                job_id: Some(job_id),
                status: ResultStatus::ErrorAndRetry as i32,
                output: Some(ResultOutput {
                    items: b"worker instance was lost before job completion".to_vec(),
                }),
                start_time: candidate.start_time.unwrap_or(now),
                end_time: now,
                worker_id: Some(worker_id),
                args: job_data.args,
                uniq_key: job_data.uniq_key,
                retried: job_data.retried,
                max_retry: resolved
                    .retry_policy
                    .as_ref()
                    .map(|policy| policy.max_retry)
                    .unwrap_or_default(),
                priority: job_data.priority,
                timeout: job_data.timeout,
                streaming_type: job_data.streaming_type,
                enqueue_time: job_data.enqueue_time,
                run_after_time: job_data.run_after_time,
                response_type: resolved.response_type,
                store_success: resolved.store_success,
                store_failure: resolved.store_failure,
                worker_name: worker_data.name.clone(),
                using: job_data.using,
                broadcast_results: resolved.broadcast_results,
                resolved_retry_policy: resolved.retry_policy,
            };
            let result_id = JobResultId {
                value: self.id_generator.generate_id()?,
            };
            // Recovery has no output stream to publish. Persist (or emit the
            // configured non-persistent failure path) before making a retry
            // visible, so a retry publication failure never hides the worker
            // loss that caused it.
            if let Err(error) = self
                .app_module
                .job_result_app
                .create_job_result_if_necessary(
                    &result_id,
                    &result_data,
                    result_data.broadcast_results,
                )
                .await
            {
                let _ = self.status.restore_running(&claim).await;
                return Err(error);
            }
            let completion = if let Some(retry_job) =
                Self::build_retry_job(&result_data, &worker_data, &metadata)
            {
                let retry_data = retry_job
                    .data
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("recovery retry job has no data"))?;
                let rdb_only_retry = retry_data.run_after_time > 0
                    || worker_data.periodic_interval > 0
                    || worker_data.queue_type == QueueType::DbOnly as i32;
                if rdb_only_retry {
                    let rdb = self
                        .app_module
                        .repositories
                        .rdb_module
                        .as_ref()
                        .ok_or_else(|| {
                            anyhow::anyhow!("RDB repository is unavailable for recovery retry")
                        })?;
                    rdb.job_repository
                        .upsert_with_recovery_claim_reset(
                            &self.status,
                            &claim,
                            &retry_job.id.expect("retry builder sets job ID"),
                            retry_data,
                        )
                        .await
                        .map(|_| None)
                } else {
                    // Normal/WithBackup queues must use the existing queue
                    // publisher. Their status reset remains guarded by the
                    // recovery claim before publication.
                    if self.status.reset_claim_to_pending(&claim).await?
                        != infra::infra::job::status::execution::ClaimOutcome::Claimed
                    {
                        anyhow::bail!("lost recovery status claim before retry publication");
                    }
                    self.app_module
                        .job_app
                        .update_job(&retry_job)
                        .await
                        .map(|_| None)
                }
            } else {
                self.app_module
                    .job_app
                    .complete_job(&result_id, &result_data, None)
                    .await
                    .map(|(_, receiver)| receiver)
            };
            if let Err(error) = completion.map(|_| ()) {
                let _ = self.status.restore_running(&claim).await;
                return Err(error);
            }
        }
        Ok(())
    }
}

impl JobBuilder for WorkerInstanceRecoveryCoordinator {}
