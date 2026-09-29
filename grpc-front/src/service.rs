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

use std::collections::HashMap;

/// Trace the request without ever passing authentication metadata to a Debug renderer.
/// Keep only validated W3C parent context and a bounded route path so existing distributed
/// tracing remains linked. The borrowed body avoids cloning potentially large Worker arguments.
pub(crate) fn without_metadata<T>(request: &tonic::Request<T>) -> tonic::Request<&T> {
    let mut view = tonic::Request::new(request.get_ref());
    if let Some(parent) = request.metadata().get("traceparent")
        && parent.to_str().is_ok_and(valid_traceparent)
    {
        view.metadata_mut().insert("traceparent", parent.clone());
    }
    if let Some(path) = request.metadata().get("path")
        && path.to_str().is_ok_and(|path| {
            path.len() <= 256
                && path.starts_with('/')
                && path
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_'))
        })
    {
        view.metadata_mut().insert("path", path.clone());
    }
    view
}

fn valid_traceparent(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 55
        && bytes[2] == b'-'
        && bytes[35] == b'-'
        && bytes[52] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 2 | 35 | 52) || byte.is_ascii_hexdigit())
}

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

// XXX simple authentication for jobworkerp metadata (authentication env and header use the same value)
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
    } // no env AUTH_TOKEN, skip authentication for backward compatibility
    Ok(jobworkerp_metadata)
}

#[cfg(test)]
mod trace_safety_tests {
    #[test]
    fn tracing_view_cannot_render_the_jobworkerp_auth_header() {
        let mut request = tonic::Request::new("public request body");
        request.metadata_mut().insert(
            "jobworkerp-auth",
            "private-auth-token".parse().expect("metadata value"),
        );
        request.metadata_mut().insert(
            "traceparent",
            "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01"
                .parse()
                .expect("traceparent"),
        );
        request
            .metadata_mut()
            .insert("path", "/v1/jobs".parse().expect("path"));
        let rendered = format!("{:?}", super::without_metadata(&request));
        assert!(rendered.contains("public request body"));
        assert!(rendered.contains("0123456789abcdef0123456789abcdef"));
        assert!(rendered.contains("/v1/jobs"));
        assert!(!rendered.contains("private-auth-token"));
        assert!(!rendered.contains("jobworkerp-auth"));
    }
}
