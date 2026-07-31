use crate::app::JobBuilder;
use crate::module::AppModule;
use anyhow::Result;
use infra::infra::IdGeneratorWrapper;
use infra::infra::job::queue::rdb::RdbJobQueueRepository;
use infra::infra::job::rdb::RdbJobRepository;
use infra::infra::job::status::execution::RdbJobStatusExecutionRepository;
use infra::infra::job::status::execution::RunningStatusCandidate;
use infra::infra::worker_instance::{ExpiredWorkerInstance, WorkerInstanceRecoveryRepository};
use jobworkerp_base::WORKER_INSTANCE_CONFIG;
use proto::jobworkerp::data::{
    JobResult, JobResultData, JobResultId, QueueType, ResultOutput, ResultStatus,
};
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

const RECOVERY_BATCH_SIZE: i64 = 1_000;
const RDB_STATUS_RECOVERY_PROTOCOL_VERSION: u32 = 1;

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
            if Self::is_recovery_participant(&expired.instance) {
                self.recover_instance(expired, timeout_millis).await?;
            } else if !self
                .registry
                .delete_expired_observed(&expired, timeout_millis)
                .await?
            {
                tracing::debug!(
                    instance_id = expired.instance.id.as_ref().map(|id| id.value),
                    "nonparticipating expired worker instance changed before legacy cleanup"
                );
            }
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
            .recover_locked(&expired, timeout_millis, &recovery_id, lock_ttl_millis)
            .await;
        match outcome {
            Ok(true) => {
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
            Ok(false) => {
                self.registry
                    .release_recovery_lock(instance_id, &recovery_id)
                    .await?;
                tracing::debug!(
                    instance_id,
                    "retaining expired worker instance because recovery candidates remain"
                );
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
        instance_timeout_millis: i64,
        recovery_id: &str,
        lock_ttl_millis: i64,
    ) -> Result<bool> {
        let instance_id = expired
            .instance
            .id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("expired worker instance has no ID"))?
            .value;
        let unbounded_recovery_after_millis = expired
            .observed_heartbeat_millis
            .saturating_add(instance_timeout_millis)
            .saturating_add(i64::try_from(
                WORKER_INSTANCE_CONFIG
                    .rdb_status_recovery
                    .unbounded_execution_recovery_timeout_sec
                    .saturating_mul(1000),
            )?);
        let mut cursor = 0;
        loop {
            let candidates = self
                .status
                .find_running_by_instance(instance_id, cursor, RECOVERY_BATCH_SIZE)
                .await?;
            if candidates.is_empty() {
                break;
            }
            for candidate in candidates {
                cursor = Self::next_recovery_cursor(cursor, candidate.job_id);
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
                if !Self::is_safe_to_recover(
                    &candidate,
                    job_data.timeout,
                    WORKER_INSTANCE_CONFIG
                        .rdb_status_recovery
                        .execution_completion_reserve_sec
                        .saturating_mul(1000),
                    unbounded_recovery_after_millis,
                    command_utils::util::datetime::now_millis(),
                ) {
                    tracing::debug!(
                        job_id = job_id.value,
                        "deferring recovery until the old execution can no longer be running"
                    );
                    continue;
                }
                let worker_id = RdbJobStatusExecutionRepository::candidate_worker_id(&candidate);
                let Some(worker_data) = self.app_module.worker_app.find_data(&worker_id).await?
                else {
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
                let mut retry_job = Self::build_retry_job(&result_data, &worker_data, &metadata);
                let result_already_saved = if retry_job.is_some() {
                    match self
                        .app_module
                        .job_result_app
                        .find_job_result_list_by_job_id(&job_id)
                        .await
                    {
                        Ok(results) => Self::has_recovery_failure_result(&results, &result_data),
                        Err(error) => {
                            let _ = self.status.restore_running(&claim).await;
                            return Err(error);
                        }
                    }
                } else {
                    false
                };
                let result_id = JobResultId {
                    value: self.id_generator.generate_id()?,
                };
                // Recovery has no output stream to publish. Persist (or emit the
                // configured non-persistent failure path) before making a retry
                // visible, so a retry publication failure never hides the worker
                // loss that caused it.
                if !result_already_saved
                    && let Err(error) = self
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
                let completion = if let Some(retry_job) = retry_job.as_mut() {
                    let retry_job_id = *Self::retry_job_id(retry_job)?;
                    let retry_data = retry_job
                        .data
                        .as_mut()
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
                                &retry_job_id,
                                retry_data,
                            )
                            .await
                            .map(|_| None)
                    } else {
                        if worker_data.queue_type == QueueType::WithBackup as i32 {
                            let rdb = self
                                .app_module
                                .repositories
                                .rdb_module
                                .as_ref()
                                .ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "RDB repository is unavailable for WithBackup recovery retry"
                                    )
                                })?;
                            let Some(reserved_until) = rdb
                                .job_repository
                                .grab_job_with_lease(
                                    &job_id,
                                    Some(job_data.timeout),
                                    job_data.grabbed_until_time.unwrap_or_default(),
                                )
                                .await?
                            else {
                                let _ = self.status.restore_running(&claim).await;
                                anyhow::bail!(
                                    "lost WithBackup job lease before retry publication: {}",
                                    job_id.value
                                );
                            };
                            // Keep the RDB queue ineligible until the Redis retry is
                            // consumed. The Redis dispatcher uses this exact lease as
                            // its compare-and-set value, transferring ownership safely.
                            Self::set_retry_grab_lease(retry_job, reserved_until)?;
                        }
                        // Normal/WithBackup queues must use the existing queue
                        // publisher. Their status reset remains guarded by the
                        // recovery claim before publication.
                        if self.status.reset_claim_to_pending(&claim).await?
                            != infra::infra::job::status::execution::ClaimOutcome::Claimed
                        {
                            anyhow::bail!("lost recovery status claim before retry publication");
                        }
                        let publication = self.app_module.job_app.update_job(retry_job).await;
                        if let Err(error) = publication {
                            match self.status.restore_pending_claim_to_running(&claim).await {
                                Ok(infra::infra::job::status::execution::ClaimOutcome::Claimed) => {
                                    tracing::warn!(
                                        job_id = claim.candidate.job_id,
                                        "restored lost execution after Redis retry publication failed"
                                    );
                                }
                                Ok(
                                    infra::infra::job::status::execution::ClaimOutcome::Conflict,
                                ) => {
                                    tracing::warn!(
                                        job_id = claim.candidate.job_id,
                                        "could not restore lost execution after Redis retry publication failed"
                                    );
                                }
                                Err(restore_error) => {
                                    tracing::warn!(
                                        job_id = claim.candidate.job_id,
                                        %restore_error,
                                        "failed to restore lost execution after Redis retry publication failed"
                                    );
                                }
                            }
                            return Err(error);
                        }
                        Ok(None)
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
        }
        if !self
            .registry
            .refresh_recovery_lock(instance_id, recovery_id, lock_ttl_millis)
            .await?
        {
            anyhow::bail!("lost recovery lock for worker instance {instance_id}");
        }
        Ok(self
            .status
            .find_running_by_instance(instance_id, 0, 1)
            .await?
            .is_empty())
    }

    fn is_safe_to_recover(
        candidate: &RunningStatusCandidate,
        job_timeout_millis: u64,
        completion_reserve_millis: u64,
        unbounded_recovery_after_millis: i64,
        now_millis: i64,
    ) -> bool {
        if job_timeout_millis == 0 {
            return now_millis >= unbounded_recovery_after_millis;
        }
        let Some(start_time) = candidate.start_time else {
            return false;
        };
        let Ok(job_timeout_millis) = i64::try_from(job_timeout_millis) else {
            return false;
        };
        let Ok(completion_reserve_millis) = i64::try_from(completion_reserve_millis) else {
            return false;
        };
        now_millis
            >= start_time
                .saturating_add(job_timeout_millis)
                .saturating_add(completion_reserve_millis)
    }

    fn set_retry_grab_lease(
        retry_job: &mut proto::jobworkerp::data::Job,
        lease: i64,
    ) -> Result<()> {
        retry_job
            .data
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("recovery retry job has no data"))?
            .grabbed_until_time = Some(lease);
        Ok(())
    }

    fn is_recovery_participant(instance: &proto::jobworkerp::data::WorkerInstance) -> bool {
        instance.data.as_ref().is_some_and(|data| {
            data.rdb_status_index_recovery_version >= RDB_STATUS_RECOVERY_PROTOCOL_VERSION
        })
    }

    fn retry_job_id(
        retry_job: &proto::jobworkerp::data::Job,
    ) -> Result<&proto::jobworkerp::data::JobId> {
        retry_job
            .id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("recovery retry job has no ID"))
    }

    fn next_recovery_cursor(current_cursor: i64, candidate_job_id: i64) -> i64 {
        current_cursor.max(candidate_job_id)
    }

    fn has_recovery_failure_result(results: &[JobResult], result: &JobResultData) -> bool {
        results.iter().any(|existing| {
            let Some(data) = existing.data.as_ref() else {
                return false;
            };
            data.status == ResultStatus::ErrorAndRetry as i32
                && data.worker_id == result.worker_id
                && data.start_time == result.start_time
        })
    }
}

impl JobBuilder for WorkerInstanceRecoveryCoordinator {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_job_carries_the_reserved_with_backup_lease() {
        let mut retry_job = proto::jobworkerp::data::Job {
            data: Some(proto::jobworkerp::data::JobData::default()),
            ..Default::default()
        };

        WorkerInstanceRecoveryCoordinator::set_retry_grab_lease(&mut retry_job, 12_345)
            .expect("retry job has data");

        assert_eq!(retry_job.data.unwrap().grabbed_until_time, Some(12_345));
    }

    #[test]
    fn retry_job_without_data_cannot_carry_a_reserved_lease() {
        let mut retry_job = proto::jobworkerp::data::Job::default();

        assert!(
            WorkerInstanceRecoveryCoordinator::set_retry_grab_lease(&mut retry_job, 12_345)
                .is_err()
        );
    }

    #[test]
    fn retry_job_without_id_is_reported_as_an_error() {
        assert!(
            WorkerInstanceRecoveryCoordinator::retry_job_id(
                &proto::jobworkerp::data::Job::default()
            )
            .is_err()
        );
    }

    #[test]
    fn only_current_protocol_instances_are_recovery_participants() {
        let participant = proto::jobworkerp::data::WorkerInstance {
            data: Some(proto::jobworkerp::data::WorkerInstanceData {
                rdb_status_index_recovery_version: 1,
                ..Default::default()
            }),
            ..Default::default()
        };

        assert!(WorkerInstanceRecoveryCoordinator::is_recovery_participant(
            &participant
        ));
        assert!(!WorkerInstanceRecoveryCoordinator::is_recovery_participant(
            &proto::jobworkerp::data::WorkerInstance::default()
        ));
    }

    #[test]
    fn recovery_failure_result_is_matched_to_the_lost_execution() {
        let result = JobResultData {
            worker_id: Some(proto::jobworkerp::data::WorkerId { value: 2 }),
            start_time: 1_000,
            ..Default::default()
        };
        let matching = JobResult {
            data: Some(JobResultData {
                status: ResultStatus::ErrorAndRetry as i32,
                worker_id: result.worker_id,
                start_time: result.start_time,
                ..Default::default()
            }),
            ..Default::default()
        };
        let different_execution = JobResult {
            data: Some(JobResultData {
                status: ResultStatus::ErrorAndRetry as i32,
                worker_id: result.worker_id,
                start_time: result.start_time + 1,
                ..Default::default()
            }),
            ..Default::default()
        };

        assert!(
            WorkerInstanceRecoveryCoordinator::has_recovery_failure_result(&[matching], &result)
        );
        assert!(
            !WorkerInstanceRecoveryCoordinator::has_recovery_failure_result(
                &[different_execution],
                &result
            )
        );
    }

    #[test]
    fn recovery_waits_for_the_job_timeout_and_completion_reserve() {
        let candidate = RunningStatusCandidate {
            job_id: 1,
            worker_id: 2,
            worker_instance_id: 3,
            version: 1,
            start_time: Some(1_000),
            updated_at: 1_000,
        };

        assert!(!WorkerInstanceRecoveryCoordinator::is_safe_to_recover(
            &candidate, 10_000, 5_000, 20_000, 15_999
        ));
        assert!(WorkerInstanceRecoveryCoordinator::is_safe_to_recover(
            &candidate, 10_000, 5_000, 20_000, 16_000
        ));
    }

    #[test]
    fn recovery_finalizes_an_unbounded_running_job_after_loss_grace() {
        let candidate = RunningStatusCandidate {
            job_id: 1,
            worker_id: 2,
            worker_instance_id: 3,
            version: 1,
            start_time: Some(1_000),
            updated_at: 1_000,
        };

        assert!(!WorkerInstanceRecoveryCoordinator::is_safe_to_recover(
            &candidate, 0, 5_000, 20_000, 19_999
        ));
        assert!(WorkerInstanceRecoveryCoordinator::is_safe_to_recover(
            &candidate, 0, 5_000, 20_000, 20_000
        ));
    }

    #[test]
    fn recovery_cursor_advances_past_each_page() {
        assert_eq!(
            WorkerInstanceRecoveryCoordinator::next_recovery_cursor(10, 15),
            15
        );
    }
}
