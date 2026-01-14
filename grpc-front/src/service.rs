pub mod error_handle;
pub mod function;
pub mod function_set;
pub mod job;
pub mod job_restore;
pub mod job_result;
pub mod job_status;
pub mod runner;
pub mod validation;
pub mod worker;
pub mod worker_instance;
pub mod checkpoint;

use std::collections::HashMap;

pub const JOB_RESULT_HEADER_NAME: &str = "x-job-result-bin";
pub const JOB_ID_HEADER_NAME: &str = "x-job-id-bin";
// prefix for jobworkerp metadata(closed headers from runner)
const JOBWORKERP_HEADER_PREFIX: &str = "jobworkerp-";

// divide metadata to HashMap (for jobworkerp metadata, inner use)
#[allow(clippy::result_large_err)]
pub fn process_metadata(
    metadata: tonic::metadata::MetadataMap,
) -> Result<HashMap<String, String>, tonic::Status> {
    let mut jobworkerp_metadata = HashMap::new();
    let mut other_metadata = HashMap::new();

    for (key, value) in metadata.into_headers().iter() {
        let key_str = key.as_str();

        if key_str.starts_with(JOBWORKERP_HEADER_PREFIX) {
            // jobworkerp metadata
            let clean_key = key_str
                .trim_start_matches(JOBWORKERP_HEADER_PREFIX)
                .to_string();
            if let Ok(val) = value.to_str() {
                jobworkerp_metadata.insert(clean_key, val.to_string());
            }
        } else {
            // other metadata
            if let Ok(val) = value.to_str() {
                other_metadata.insert(key_str.to_string(), val.to_string());
            }
        }
    }
    process_jobworkerp_metadata(jobworkerp_metadata)?;

    Ok(other_metadata)
}

// XXX simple authentication for jobworkerp metadata (auhthentication env and header is same value)
const AUTH_TOKEN_ENV_KEY: &str = "AUTH_TOKEN";
#[allow(clippy::result_large_err)]
fn process_jobworkerp_metadata(
    jobworkerp_metadata: HashMap<String, String>,
) -> Result<HashMap<String, String>, tonic::Status> {
    if let Ok(auth_token) = std::env::var(AUTH_TOKEN_ENV_KEY) {
        if let Some(token) = jobworkerp_metadata.get("auth") {
            if token != &auth_token {
                return Err(tonic::Status::unauthenticated("Invalid auth token"));
            }
        } else {
            return Err(tonic::Status::unauthenticated("Missing auth token"));
        }
    } // no env AUTH_TOKEN, skip authentication
    Ok(jobworkerp_metadata)
}
