use anyhow::{Result, ensure};
use prost::Message;
use sha2::{Digest, Sha256};

use crate::jobworkerp::data::{
    SandboxExecutionAnomalyCode, SandboxExecutionEndState, SandboxExecutionObservation,
    SandboxExecutionProducerState,
};

pub const SANDBOX_OBSERVATION_SCHEMA_VERSION: u32 = 1;
pub const MAX_SANDBOX_OBSERVATION_BYTES: usize = 64 * 1024;
pub const MAX_SANDBOX_OBSERVATION_ANOMALIES: usize = 32;
const MAX_SANDBOX_OBSERVATION_USING_BYTES: usize = 255;
const SANDBOX_OBSERVATION_DIGEST_DOMAIN: &[u8] = b"jobworkerp.sandbox_execution_observation.v1\0";

impl SandboxExecutionObservation {
    /// Seal an observation using a deterministic, row-identity-bound protobuf digest.
    pub fn seal(&mut self) -> Result<()> {
        self.observation_sha256.clear();
        self.validate_shape()?;
        let canonical = self.canonical_bytes()?;
        let digest = digest_with_domain(SANDBOX_OBSERVATION_DIGEST_DOMAIN, &canonical);
        self.observation_sha256 = digest.to_vec();
        self.validate()
    }

    /// Check all source-shape invariants and the observation digest.
    pub fn validate(&self) -> Result<()> {
        self.validate_shape()?;
        ensure!(
            self.observation_sha256.len() == 32,
            "sandbox observation digest must contain 32 bytes"
        );
        let canonical = self.canonical_bytes()?;
        let expected = digest_with_domain(SANDBOX_OBSERVATION_DIGEST_DOMAIN, &canonical);
        ensure!(
            self.observation_sha256.as_slice() == expected,
            "sandbox observation digest mismatch"
        );
        Ok(())
    }

    /// Encode only observations within the storage limit.
    pub fn encode_validated(&self) -> Result<Vec<u8>> {
        self.validate()?;
        ensure!(
            self.encoded_len() <= MAX_SANDBOX_OBSERVATION_BYTES,
            "sandbox observation exceeds the 64 KiB storage limit"
        );
        Ok(self.encode_to_vec())
    }

    /// Reject oversized blobs before decoding untrusted database bytes.
    pub fn decode_validated(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_SANDBOX_OBSERVATION_BYTES,
            "sandbox observation exceeds the 64 KiB storage limit"
        );
        let observation = Self::decode(bytes)?;
        observation.validate()?;
        Ok(observation)
    }

    fn validate_shape(&self) -> Result<()> {
        ensure!(
            self.schema_version == SANDBOX_OBSERVATION_SCHEMA_VERSION,
            "unsupported sandbox observation schema version"
        );
        ensure!(
            self.job_id > 0,
            "sandbox observation job ID must be positive"
        );
        ensure!(
            self.worker_id > 0,
            "sandbox observation worker ID must be positive"
        );
        ensure!(
            self.runner_id > 0,
            "sandbox observation runner ID must be positive"
        );
        ensure!(
            self.result_id > 0,
            "sandbox observation result ID must be positive"
        );
        for (name, digest) in [
            ("dispatch args", self.dispatch_args_sha256.as_slice()),
            ("worker settings", self.worker_settings_sha256.as_slice()),
            ("method schema", self.method_schema_sha256.as_slice()),
            ("host settings", self.host_settings_sha256.as_slice()),
        ] {
            ensure!(digest.len() == 32, "{name} digest must contain 32 bytes");
        }
        ensure!(
            self.using.len() <= MAX_SANDBOX_OBSERVATION_USING_BYTES,
            "sandbox observation using value exceeds its length limit"
        );
        ensure!(
            crate::jobworkerp::data::ResultStatus::try_from(self.stored_result_status).is_ok(),
            "sandbox observation contains an unknown stored result status"
        );
        ensure!(
            SandboxExecutionEndState::try_from(self.end_state).is_ok(),
            "sandbox observation contains an unknown end state"
        );
        ensure!(
            SandboxExecutionProducerState::try_from(self.producer_state).is_ok(),
            "sandbox observation contains an unknown producer state"
        );
        ensure!(
            self.anomaly_codes.len() <= MAX_SANDBOX_OBSERVATION_ANOMALIES,
            "sandbox observation contains too many anomaly codes"
        );
        for code in &self.anomaly_codes {
            ensure!(
                SandboxExecutionAnomalyCode::try_from(*code)
                    .is_ok_and(|code| code != SandboxExecutionAnomalyCode::Unspecified),
                "sandbox observation contains an unknown or unspecified anomaly code"
            );
        }
        for (name, digest) in [
            ("stdout", self.stdout_sha256.as_ref()),
            ("stderr", self.stderr_sha256.as_ref()),
            ("trailer", self.trailer_sha256.as_ref()),
        ] {
            if let Some(digest) = digest {
                ensure!(digest.len() == 32, "{name} digest must contain 32 bytes");
            }
        }
        if SandboxExecutionProducerState::try_from(self.producer_state)?
            == SandboxExecutionProducerState::Clean
        {
            ensure!(
                SandboxExecutionEndState::try_from(self.end_state)?
                    == SandboxExecutionEndState::Normal
                    && self.cli_exit_code.is_some()
                    && self.anomaly_codes.is_empty(),
                "clean sandbox observation requires a normal end, exit code, and no anomalies"
            );
        }
        ensure!(
            self.encoded_len() <= MAX_SANDBOX_OBSERVATION_BYTES,
            "sandbox observation exceeds the 64 KiB storage limit"
        );
        Ok(())
    }

    fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let mut canonical = self.clone();
        canonical.observation_sha256.clear();
        ensure!(
            canonical.encoded_len() + 34 <= MAX_SANDBOX_OBSERVATION_BYTES,
            "sandbox observation exceeds the 64 KiB storage limit"
        );
        Ok(canonical.encode_to_vec())
    }
}

pub fn sha256_digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn digest_with_domain(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(bytes);
    digest.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::jobworkerp::data::{JobResult, JobResultData, JobResultId, ResultOutput};
    use prost::Message;

    #[derive(Clone, PartialEq, Message)]
    struct OldJobResult {
        #[prost(message, optional, tag = "1")]
        id: Option<JobResultId>,
        #[prost(message, optional, tag = "2")]
        data: Option<JobResultData>,
        #[prost(map = "string, string", tag = "3")]
        metadata: HashMap<String, String>,
    }

    fn valid_observation() -> SandboxExecutionObservation {
        let mut observation = SandboxExecutionObservation {
            schema_version: SANDBOX_OBSERVATION_SCHEMA_VERSION,
            job_id: 31,
            worker_id: 32,
            runner_id: 33,
            result_id: 34,
            dispatch_args_sha256: sha256_digest(b"args").to_vec(),
            worker_settings_sha256: sha256_digest(b"worker settings").to_vec(),
            method_schema_sha256: sha256_digest(b"method schema").to_vec(),
            host_settings_sha256: sha256_digest(b"host settings").to_vec(),
            using: "run".to_owned(),
            retry_ordinal: 2,
            stored_result_status: crate::jobworkerp::data::ResultStatus::Success as i32,
            cli_exit_code: Some(7),
            end_state: SandboxExecutionEndState::Normal as i32,
            producer_state: SandboxExecutionProducerState::Unknown as i32,
            anomaly_codes: Vec::new(),
            stdout_bytes: Some(4),
            stderr_bytes: None,
            stdout_sha256: Some(sha256_digest(b"out").to_vec()),
            stderr_sha256: None,
            trailer_bytes: None,
            trailer_sha256: None,
            observation_sha256: Vec::new(),
        };
        observation.seal().unwrap();
        observation
    }

    #[test]
    fn observation_seal_is_deterministic_and_binds_row_identity() {
        let observation = valid_observation();
        observation.validate().unwrap();
        let encoded = observation.encode_validated().unwrap();
        assert_eq!(
            SandboxExecutionObservation::decode_validated(&encoded).unwrap(),
            observation
        );

        let mut changed = observation.clone();
        changed.worker_id += 1;
        assert!(changed.validate().is_err());
    }

    #[test]
    fn observations_preserve_missing_empty_and_nonempty_result_output() {
        let observation = valid_observation();
        for output in [
            None,
            Some(ResultOutput { items: Vec::new() }),
            Some(ResultOutput {
                items: b"result bytes".to_vec(),
            }),
        ] {
            let result = JobResult {
                id: Some(JobResultId { value: 34 }),
                data: Some(JobResultData {
                    output: output.clone(),
                    ..Default::default()
                }),
                sandbox_execution_observation: Some(observation.clone()),
                ..Default::default()
            };
            let decoded = JobResult::decode(result.encode_to_vec().as_slice()).unwrap();
            assert_eq!(decoded.data.unwrap().output, output);
            assert_eq!(
                decoded.sandbox_execution_observation,
                Some(observation.clone())
            );
        }
    }

    #[test]
    fn old_job_result_reader_ignores_outer_observation_without_using_output_slot() {
        let new_result = JobResult {
            id: Some(JobResultId { value: 34 }),
            data: Some(JobResultData {
                output: None,
                ..Default::default()
            }),
            metadata: HashMap::from([("source".to_owned(), "rdb".to_owned())]),
            sandbox_execution_observation: Some(valid_observation()),
        };

        let old_reader = OldJobResult::decode(new_result.encode_to_vec().as_slice()).unwrap();
        assert_eq!(old_reader.id, new_result.id);
        assert_eq!(old_reader.data, new_result.data);
        assert_eq!(old_reader.metadata, new_result.metadata);
        assert_eq!(old_reader.data.as_ref().unwrap().output, None);

        let old_round_trip = JobResult::decode(old_reader.encode_to_vec().as_slice()).unwrap();
        assert!(old_round_trip.sandbox_execution_observation.is_none());
    }

    #[test]
    fn observation_rejects_unknown_schema_bad_digest_invalid_ids_and_bad_facts() {
        let valid = valid_observation();

        let mut future_schema = valid.clone();
        future_schema.schema_version += 1;
        future_schema.seal().unwrap_err();

        let mut bad_digest = valid.clone();
        bad_digest.observation_sha256[0] ^= 0xff;
        assert!(bad_digest.validate().is_err());

        let mut invalid_id = valid.clone();
        invalid_id.job_id = 0;
        invalid_id.seal().unwrap_err();

        let mut clean_nonzero_exit = valid;
        clean_nonzero_exit.producer_state = SandboxExecutionProducerState::Clean as i32;
        clean_nonzero_exit.seal().unwrap();
        assert_eq!(clean_nonzero_exit.cli_exit_code, Some(7));

        let mut clean_missing_exit = clean_nonzero_exit;
        clean_missing_exit.cli_exit_code = None;
        assert!(clean_missing_exit.seal().is_err());
    }

    #[test]
    fn observation_decode_enforces_byte_limit_before_protobuf_decode() {
        let oversized = vec![0xff; MAX_SANDBOX_OBSERVATION_BYTES + 1];
        let error = SandboxExecutionObservation::decode_validated(&oversized).unwrap_err();
        assert!(error.to_string().contains("64 KiB"));
    }

    #[test]
    fn sha256_digest_matches_standard_sha256() {
        assert_eq!(
            sha256_digest(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }
}
