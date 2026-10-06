use std::collections::HashMap;

use infra::infra::job_result::rdb::{
    RdbJobResultRepository, RdbJobResultRepositoryImpl, SandboxObservationStoreResult,
};
use prost::Message;
use proto::jobworkerp::data::{
    JobId, JobResult, JobResultData, JobResultId, ResultOutput, ResultStatus,
    SandboxExecutionAnomalyCode, SandboxExecutionEndState, SandboxExecutionObservation,
    SandboxExecutionProducerState, WorkerId,
};
use proto::sandbox_observation::sha256_digest;
use sqlx::{Row, migrate::Migrator, sqlite::SqlitePoolOptions};

const OBSERVATION_MIGRATION: &str =
    include_str!("../sql/migrations/sqlite/005_sandbox_execution_observation.sql");

#[derive(Clone, PartialEq, Message)]
struct OldJobResult {
    #[prost(message, optional, tag = "1")]
    id: Option<JobResultId>,
    #[prost(message, optional, tag = "2")]
    data: Option<JobResultData>,
    #[prost(map = "string, string", tag = "3")]
    metadata: HashMap<String, String>,
}

#[tokio::test]
async fn observation_migration_keeps_legacy_result_rows_and_adds_nullable_storage() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE job_result (id INTEGER PRIMARY KEY, output BLOB NOT NULL);\
         INSERT INTO job_result (id, output) VALUES (19, X'0A00');",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::raw_sql(OBSERVATION_MIGRATION)
        .execute(&pool)
        .await
        .unwrap();

    let row =
        sqlx::query("SELECT output, sandbox_execution_observation FROM job_result WHERE id = 19")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.get::<Vec<u8>, _>("output"), [0x0a, 0x00]);
    assert!(
        row.get::<Option<Vec<u8>>, _>("sandbox_execution_observation")
            .is_none()
    );

    let column = sqlx::query("PRAGMA table_info(job_result)")
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.get::<String, _>("name") == "sandbox_execution_observation")
        .unwrap();
    assert_eq!(column.get::<i64, _>("notnull"), 0);
}

#[tokio::test]
async fn sqlite_migration_history_includes_observation_migration() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let migration_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sql/migrations/sqlite");
    Migrator::new(migration_dir)
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();

    let version: i64 =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(version, 5);
}

#[tokio::test]
async fn observation_cas_is_idempotent_bound_to_row_and_keeps_output_unchanged() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let migration_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sql/migrations/sqlite");
    Migrator::new(migration_dir)
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    let pool: &'static sqlx::SqlitePool = Box::leak(Box::new(pool));
    let repository = RdbJobResultRepositoryImpl::new(pool);

    let args = b"raw serialized args";
    let outputs = [
        None,
        Some(ResultOutput { items: Vec::new() }),
        Some(ResultOutput {
            items: b"original output".to_vec(),
        }),
    ];
    for (index, output) in outputs.into_iter().enumerate() {
        let id = JobResultId {
            value: 51_000 + index as i64,
        };
        let job_id = 52_000 + index as i64;
        let worker_id = 53_000 + index as i64;
        let data = JobResultData {
            job_id: Some(JobId { value: job_id }),
            worker_id: Some(WorkerId { value: worker_id }),
            args: args.to_vec(),
            status: ResultStatus::Success as i32,
            output,
            retried: 3,
            using: Some("run".to_owned()),
            ..Default::default()
        };
        assert!(repository.create(&id, &data).await.unwrap());
        let before = repository.find(&id).await.unwrap().unwrap();
        let before_output = before.data.as_ref().unwrap().output.clone();
        let before_output_blob: Vec<u8> =
            sqlx::query_scalar("SELECT output FROM job_result WHERE id = ?")
                .bind(id.value)
                .fetch_one(pool)
                .await
                .unwrap();

        let observation = observation_for(id.value, job_id, worker_id, args, 3);
        assert_eq!(
            repository
                .finalize_sandbox_observation(&id, &observation)
                .await
                .unwrap(),
            SandboxObservationStoreResult::Stored
        );
        let after = repository.find(&id).await.unwrap().unwrap();
        assert_eq!(
            after.sandbox_execution_observation,
            Some(observation.clone())
        );
        assert_eq!(after.data.as_ref().unwrap().output, before_output);
        let after_output_blob: Vec<u8> =
            sqlx::query_scalar("SELECT output FROM job_result WHERE id = ?")
                .bind(id.value)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(after_output_blob, before_output_blob);
        assert_eq!(
            repository
                .finalize_sandbox_observation(&id, &observation)
                .await
                .unwrap(),
            SandboxObservationStoreResult::AlreadyIdentical
        );

        if index == 0 {
            assert_eq!(
                before.data.unwrap().output,
                Some(ResultOutput { items: Vec::new() }),
                "the old empty-blob interpretation remains unchanged"
            );
            let mut different = observation.clone();
            different.cli_exit_code = Some(8);
            different.seal().unwrap();
            assert_eq!(
                repository
                    .finalize_sandbox_observation(&id, &different)
                    .await
                    .unwrap(),
                SandboxObservationStoreResult::Conflict
            );
            let mut different_method_case = observation.clone();
            different_method_case.using = "RUN".to_owned();
            different_method_case.seal().unwrap();
            assert_eq!(
                repository
                    .finalize_sandbox_observation(&id, &different_method_case)
                    .await
                    .unwrap(),
                SandboxObservationStoreResult::Conflict,
                "method names are compared exactly, not with MySQL collation rules"
            );
        }

        if index == 2 {
            let untouched_id = JobResultId {
                value: id.value + 10_000,
            };
            let untouched_job = job_id + 10_000;
            let untouched_worker = worker_id + 10_000;
            let untouched = JobResultData {
                job_id: Some(JobId {
                    value: untouched_job,
                }),
                worker_id: Some(WorkerId {
                    value: untouched_worker,
                }),
                args: args.to_vec(),
                status: ResultStatus::Success as i32,
                output: Some(ResultOutput {
                    items: b"do not modify".to_vec(),
                }),
                retried: 3,
                using: None,
                ..Default::default()
            };
            assert!(repository.create(&untouched_id, &untouched).await.unwrap());
            for mismatch_field in 0..6 {
                let mut mismatch =
                    observation_for(untouched_id.value, untouched_job, untouched_worker, args, 3);
                match mismatch_field {
                    0 => mismatch.job_id += 1,
                    1 => mismatch.worker_id += 1,
                    2 => mismatch.dispatch_args_sha256 = sha256_digest(b"different args").to_vec(),
                    3 => mismatch.stored_result_status = ResultStatus::FatalError as i32,
                    4 => mismatch.using = "other-method".to_owned(),
                    _ => mismatch.retry_ordinal += 1,
                }
                mismatch.seal().unwrap();
                assert_eq!(
                    repository
                        .finalize_sandbox_observation(&untouched_id, &mismatch)
                        .await
                        .unwrap(),
                    SandboxObservationStoreResult::Conflict
                );
                let unchanged = repository.find(&untouched_id).await.unwrap().unwrap();
                assert!(unchanged.sandbox_execution_observation.is_none());
                assert_eq!(
                    unchanged.data.unwrap().output.unwrap().items,
                    b"do not modify"
                );
            }
            let default_method =
                observation_for(untouched_id.value, untouched_job, untouched_worker, args, 3);
            assert_eq!(
                repository
                    .finalize_sandbox_observation(&untouched_id, &default_method)
                    .await
                    .unwrap(),
                SandboxObservationStoreResult::Conflict,
                "absent using must not be treated as the explicit run method"
            );
            let mut absent_using = default_method;
            absent_using.using.clear();
            absent_using.seal().unwrap();
            assert_eq!(
                repository
                    .finalize_sandbox_observation(&untouched_id, &absent_using)
                    .await
                    .unwrap(),
                SandboxObservationStoreResult::Stored
            );

            let wrong_result_id = JobResultId {
                value: id.value + 1,
            };
            assert_eq!(
                repository
                    .finalize_sandbox_observation(&wrong_result_id, &observation)
                    .await
                    .unwrap(),
                SandboxObservationStoreResult::Conflict
            );
            let mut wrong_observation_id = observation.clone();
            wrong_observation_id.result_id += 1;
            wrong_observation_id.seal().unwrap();
            assert_eq!(
                repository
                    .finalize_sandbox_observation(&id, &wrong_observation_id)
                    .await
                    .unwrap(),
                SandboxObservationStoreResult::Conflict
            );
        }
    }

    let concurrent_id = JobResultId { value: 51_010 };
    let concurrent_data = JobResultData {
        job_id: Some(JobId { value: 52_010 }),
        worker_id: Some(WorkerId { value: 53_010 }),
        args: args.to_vec(),
        status: ResultStatus::Success as i32,
        output: Some(ResultOutput {
            items: b"concurrent".to_vec(),
        }),
        retried: 0,
        using: Some("run".to_owned()),
        ..Default::default()
    };
    assert!(
        repository
            .create(&concurrent_id, &concurrent_data)
            .await
            .unwrap()
    );
    let concurrent_observation = observation_for(51_010, 52_010, 53_010, args, 0);
    let (first, second) = tokio::join!(
        repository.finalize_sandbox_observation(&concurrent_id, &concurrent_observation),
        repository.finalize_sandbox_observation(&concurrent_id, &concurrent_observation),
    );
    let outcomes = [first.unwrap(), second.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == SandboxObservationStoreResult::Stored)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == SandboxObservationStoreResult::AlreadyIdentical)
            .count(),
        1
    );

    let corrupt_id = JobResultId { value: 51_020 };
    let corrupt_data = JobResultData {
        job_id: Some(JobId { value: 52_020 }),
        worker_id: Some(WorkerId { value: 53_020 }),
        args: args.to_vec(),
        status: ResultStatus::Success as i32,
        output: Some(ResultOutput {
            items: b"untouched corrupt row output".to_vec(),
        }),
        ..Default::default()
    };
    assert!(repository.create(&corrupt_id, &corrupt_data).await.unwrap());
    sqlx::query("UPDATE job_result SET sandbox_execution_observation = ? WHERE id = ?")
        .bind(vec![
            0xff;
            proto::sandbox_observation::MAX_SANDBOX_OBSERVATION_BYTES
                + 1
        ])
        .bind(corrupt_id.value)
        .execute(pool)
        .await
        .unwrap();
    assert!(repository.find(&corrupt_id).await.is_err());

    let drift_id = JobResultId { value: 51_021 };
    let drift_data = JobResultData {
        job_id: Some(JobId { value: 52_021 }),
        worker_id: Some(WorkerId { value: 53_021 }),
        args: args.to_vec(),
        status: ResultStatus::Success as i32,
        retried: 0,
        using: Some("run".to_owned()),
        ..Default::default()
    };
    assert!(repository.create(&drift_id, &drift_data).await.unwrap());
    let drift_observation = observation_for(51_021, 52_021, 53_021, args, 0);
    assert_eq!(
        repository
            .finalize_sandbox_observation(&drift_id, &drift_observation)
            .await
            .unwrap(),
        SandboxObservationStoreResult::Stored
    );
    sqlx::query("UPDATE job_result SET worker_id = worker_id + 1 WHERE id = ?")
        .bind(drift_id.value)
        .execute(pool)
        .await
        .unwrap();
    assert!(repository.find(&drift_id).await.is_err());

    let missing_id = JobResultId { value: 59_999 };
    let missing = observation_for(missing_id.value, 60_000, 60_001, args, 0);
    assert_eq!(
        repository
            .finalize_sandbox_observation(&missing_id, &missing)
            .await
            .unwrap(),
        SandboxObservationStoreResult::NotStored
    );
}

#[tokio::test]
async fn update_preserves_sealed_observation_and_rejects_changed_persisted_data() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let migration_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sql/migrations/sqlite");
    Migrator::new(migration_dir)
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    let pool: &'static sqlx::SqlitePool = Box::leak(Box::new(pool));
    let repository = RdbJobResultRepositoryImpl::new(pool);

    let id = JobResultId { value: 61_100 };
    let job_id = 62_100;
    let worker_id = 63_100;
    let args = b"sealed row args";
    let original = JobResultData {
        job_id: Some(JobId { value: job_id }),
        worker_id: Some(WorkerId { value: worker_id }),
        args: args.to_vec(),
        status: ResultStatus::Success as i32,
        output: None,
        retried: 2,
        using: Some("run".to_owned()),
        ..Default::default()
    };
    assert!(repository.create(&id, &original).await.unwrap());

    let observation_a = observation_for(id.value, job_id, worker_id, args, 2);
    assert_eq!(
        repository
            .finalize_sandbox_observation(&id, &observation_a)
            .await
            .unwrap(),
        SandboxObservationStoreResult::Stored
    );

    let mut tx = pool.begin().await.unwrap();
    assert!(repository.update(&mut tx, &id, &original).await.unwrap());
    tx.commit().await.unwrap();
    assert_eq!(
        repository
            .find(&id)
            .await
            .unwrap()
            .unwrap()
            .sandbox_execution_observation,
        Some(observation_a.clone()),
        "an identical data update must not clear the sealed observation"
    );

    let equivalent_persisted_data = JobResultData {
        output: Some(ResultOutput { items: Vec::new() }),
        worker_name: "restored worker name".to_owned(),
        store_success: true,
        store_failure: true,
        broadcast_results: true,
        max_retry: 99,
        ..original.clone()
    };
    let mut tx = pool.begin().await.unwrap();
    assert!(
        repository
            .update(&mut tx, &id, &equivalent_persisted_data)
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
    assert_eq!(
        repository
            .find(&id)
            .await
            .unwrap()
            .unwrap()
            .sandbox_execution_observation,
        Some(observation_a.clone()),
        "non-persisted worker fields and equivalent empty output do not invalidate evidence"
    );

    let mut observation_b = observation_a.clone();
    observation_b.cli_exit_code = Some(8);
    observation_b.seal().unwrap();
    assert_eq!(
        repository
            .finalize_sandbox_observation(&id, &observation_b)
            .await
            .unwrap(),
        SandboxObservationStoreResult::Conflict,
        "identical update must not reopen a sealed observation for replacement"
    );

    for changed_field in 0..8 {
        let mut changed = original.clone();
        match changed_field {
            0 => changed.args = b"different args".to_vec(),
            1 => changed.job_id = Some(JobId { value: job_id + 1 }),
            2 => {
                changed.worker_id = Some(WorkerId {
                    value: worker_id + 1,
                })
            }
            3 => changed.status = ResultStatus::FatalError as i32,
            4 => changed.retried += 1,
            5 => changed.using = Some("other-method".to_owned()),
            6 => {
                changed.output = Some(ResultOutput {
                    items: b"different output".to_vec(),
                })
            }
            _ => changed.end_time += 1,
        }
        let mut tx = pool.begin().await.unwrap();
        assert!(
            repository.update(&mut tx, &id, &changed).await.is_err(),
            "changed persisted field {changed_field} must not mutate a sealed row"
        );
        tx.rollback().await.unwrap();

        let after = repository.find(&id).await.unwrap().unwrap();
        assert_eq!(
            after.sandbox_execution_observation,
            Some(observation_a.clone())
        );
        assert_eq!(
            after.data.as_ref().unwrap().job_id,
            Some(JobId { value: job_id })
        );
        assert_eq!(
            after.data.as_ref().unwrap().worker_id,
            Some(WorkerId { value: worker_id })
        );
        assert_eq!(after.data.as_ref().unwrap().args, args);
        assert_eq!(
            after.data.as_ref().unwrap().status,
            ResultStatus::Success as i32
        );
        assert_eq!(after.data.as_ref().unwrap().retried, 2);
        assert_eq!(after.data.as_ref().unwrap().using.as_deref(), Some("run"));
        assert_eq!(
            after.data.as_ref().unwrap().output,
            Some(ResultOutput { items: Vec::new() }),
            "legacy empty-blob decoding must remain unchanged"
        );
    }
}

#[tokio::test]
async fn concurrent_update_and_finalize_never_rebind_or_unseal_observation() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let migration_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sql/migrations/sqlite");
    Migrator::new(migration_dir)
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    let pool: &'static sqlx::SqlitePool = Box::leak(Box::new(pool));
    let repository = RdbJobResultRepositoryImpl::new(pool);

    let id = JobResultId { value: 61_200 };
    let job_id = 62_200;
    let worker_id = 63_200;
    let args = b"original race args";
    let original = JobResultData {
        job_id: Some(JobId { value: job_id }),
        worker_id: Some(WorkerId { value: worker_id }),
        args: args.to_vec(),
        status: ResultStatus::Success as i32,
        retried: 1,
        using: Some("run".to_owned()),
        output: Some(ResultOutput {
            items: b"race output".to_vec(),
        }),
        ..Default::default()
    };
    assert!(repository.create(&id, &original).await.unwrap());
    let observation = observation_for(id.value, job_id, worker_id, args, 1);
    let mut changed = original.clone();
    changed.args = b"changed race args".to_vec();

    let update = async {
        let mut tx = pool.begin().await.unwrap();
        let result = repository.update(&mut tx, &id, &changed).await;
        if result.is_ok() {
            tx.commit().await.unwrap();
        } else {
            tx.rollback().await.unwrap();
        }
        result
    };
    let (update_result, finalize_result) = tokio::join!(
        update,
        repository.finalize_sandbox_observation(&id, &observation),
    );
    let finalize_result = finalize_result.unwrap();
    let stored = repository.find(&id).await.unwrap().unwrap();

    match stored.sandbox_execution_observation {
        Some(stored_observation) => {
            assert_eq!(stored_observation, observation);
            let stored_data = stored.data.unwrap();
            assert_eq!(stored_data.args, original.args);
            assert_eq!(stored_data.output, original.output);
            assert!(
                !matches!(update_result, Ok(true)),
                "an update cannot report success after finalization won the race"
            );
            assert!(matches!(
                finalize_result,
                SandboxObservationStoreResult::Stored
                    | SandboxObservationStoreResult::AlreadyIdentical
            ));
        }
        None => {
            assert_eq!(finalize_result, SandboxObservationStoreResult::Conflict);
            assert!(update_result.unwrap());
            assert_eq!(stored.data.unwrap().args, changed.args);
        }
    }
}

#[test]
fn observation_validation_and_old_wire_reader_fail_closed_without_output_reuse() {
    let mut future_schema = observation_for(34, 31, 32, b"args", 2);
    future_schema.schema_version += 1;
    assert!(future_schema.seal().is_err());

    let mut invalid_id = observation_for(34, 31, 32, b"args", 2);
    invalid_id.job_id = 0;
    assert!(invalid_id.seal().is_err());

    let mut invalid_digest = observation_for(34, 31, 32, b"args", 2);
    invalid_digest.observation_sha256[0] ^= 0xff;
    assert!(invalid_digest.validate().is_err());

    let mut unknown_anomaly = observation_for(34, 31, 32, b"args", 2);
    unknown_anomaly.anomaly_codes = vec![i32::MAX];
    assert!(unknown_anomaly.seal().is_err());

    let mut too_many_anomalies = observation_for(34, 31, 32, b"args", 2);
    too_many_anomalies.anomaly_codes = vec![
        SandboxExecutionAnomalyCode::ExitCodeMissing as i32;
        proto::sandbox_observation::MAX_SANDBOX_OBSERVATION_ANOMALIES
            + 1
    ];
    assert!(too_many_anomalies.seal().is_err());

    let mut malformed_optional_digest = observation_for(34, 31, 32, b"args", 2);
    malformed_optional_digest.stdout_sha256 = Some(vec![1, 2]);
    assert!(malformed_optional_digest.seal().is_err());

    let oversized = vec![0xff; proto::sandbox_observation::MAX_SANDBOX_OBSERVATION_BYTES + 1];
    assert!(SandboxExecutionObservation::decode_validated(&oversized).is_err());

    let mut clean = observation_for(34, 31, 32, b"args", 2);
    clean.end_state = SandboxExecutionEndState::Normal as i32;
    clean.producer_state = SandboxExecutionProducerState::Clean as i32;
    clean.cli_exit_code = Some(7);
    clean.seal().unwrap();
    assert_eq!(clean.cli_exit_code, Some(7));
    let mut clean_without_exit = clean;
    clean_without_exit.cli_exit_code = None;
    assert!(clean_without_exit.seal().is_err());

    let result_id = JobResultId { value: 34 };
    let observation = observation_for(34, 31, 32, b"args", 2);
    for output in [
        None,
        Some(ResultOutput { items: Vec::new() }),
        Some(ResultOutput {
            items: b"output bytes".to_vec(),
        }),
    ] {
        let current = JobResult {
            id: Some(result_id),
            data: Some(JobResultData {
                output: output.clone(),
                ..Default::default()
            }),
            metadata: HashMap::from([("source".to_owned(), "rdb".to_owned())]),
            sandbox_execution_observation: Some(observation.clone()),
        };
        let decoded = JobResult::decode(current.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded.data.as_ref().unwrap().output, output);
        assert_eq!(
            decoded.sandbox_execution_observation,
            Some(observation.clone())
        );

        let legacy = OldJobResult::decode(current.encode_to_vec().as_slice()).unwrap();
        assert_eq!(legacy.id, current.id);
        assert_eq!(legacy.metadata, current.metadata);
        assert_eq!(legacy.data.as_ref().unwrap().output, output);
        let old_round_trip = JobResult::decode(legacy.encode_to_vec().as_slice()).unwrap();
        assert!(old_round_trip.sandbox_execution_observation.is_none());
    }
}

fn observation_for(
    result_id: i64,
    job_id: i64,
    worker_id: i64,
    args: &[u8],
    retry_ordinal: u32,
) -> SandboxExecutionObservation {
    let mut observation = SandboxExecutionObservation {
        schema_version: 1,
        job_id,
        worker_id,
        runner_id: 54_000,
        result_id,
        dispatch_args_sha256: sha256_digest(args).to_vec(),
        worker_settings_sha256: sha256_digest(b"worker settings").to_vec(),
        method_schema_sha256: sha256_digest(b"method schema").to_vec(),
        host_settings_sha256: sha256_digest(b"host settings").to_vec(),
        using: "run".to_owned(),
        retry_ordinal,
        stored_result_status: ResultStatus::Success as i32,
        cli_exit_code: Some(7),
        end_state: SandboxExecutionEndState::Normal as i32,
        producer_state: SandboxExecutionProducerState::Unknown as i32,
        ..Default::default()
    };
    observation.seal().unwrap();
    observation
}
