use crate::jobworkerp::data::{Job, JobResult, JobResultData};
use std::fmt;

/// Log adapter for `Job` that picks the rendering granularity based on
/// the active `tracing` level.
///
/// `Job::data.args` is a `Vec<u8>` that frequently carries multi-MB
/// payloads (audio frames, file blobs). Dumping the full `Debug`
/// representation at `debug` level floods log files when many jobs flow
/// through the dispatcher. `JobSummary`'s `Display` impl checks
/// `tracing::enabled!(Level::TRACE)` and emits:
///
/// - **TRACE enabled**: the full `Debug` representation (raw args), so
///   operators who explicitly opted into trace logs still get
///   everything.
/// - **otherwise**: a compact summary (ids + `args_bytes` + streaming
///   type + retry count) sized for routine debug output.
///
/// Callers therefore only need `tracing::debug!("...: {}", JobSummary(&job))`
/// once — no per-callsite level branching.
pub struct JobSummary<'a>(pub &'a Job);

impl fmt::Display for JobSummary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if tracing::enabled!(tracing::Level::TRACE) {
            return write!(f, "{:?}", self.0);
        }
        let Job { id, data, metadata } = self.0;
        let job_id = id.as_ref().map(|i| i.value);
        let (worker_id, args_bytes, streaming_type, retried) = match data {
            Some(d) => (
                d.worker_id.as_ref().map(|w| w.value),
                d.args.len(),
                d.streaming_type,
                d.retried,
            ),
            None => (None, 0, 0, 0),
        };
        write!(
            f,
            "Job {{ id={job_id:?}, worker_id={worker_id:?}, args_bytes={args_bytes}, streaming_type={streaming_type}, retried={retried}, metadata_keys={} }}",
            metadata.len(),
        )
    }
}

/// Log adapter for `JobResultData`. Same TRACE-gated behavior as
/// [`JobSummary`]; the summary form emits `args` / `output.items` byte
/// counts plus status, timing, and retry fields for triage.
pub struct JobResultDataSummary<'a>(pub &'a JobResultData);

impl fmt::Display for JobResultDataSummary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if tracing::enabled!(tracing::Level::TRACE) {
            return write!(f, "{:?}", self.0);
        }
        let d = self.0;
        let job_id = d.job_id.as_ref().map(|i| i.value);
        let worker_id = d.worker_id.as_ref().map(|i| i.value);
        let worker_name = &d.worker_name;
        let status = d.status;
        let args_bytes = d.args.len();
        let output_bytes = d.output.as_ref().map(|o| o.items.len()).unwrap_or(0);
        let streaming_type = d.streaming_type;
        let retried = d.retried;
        let max_retry = d.max_retry;
        let start_time = d.start_time;
        let end_time = d.end_time;
        let response_type = d.response_type;
        write!(
            f,
            "JobResultData {{ job_id={job_id:?}, worker_id={worker_id:?}, worker_name={worker_name:?}, status={status}, args_bytes={args_bytes}, output_bytes={output_bytes}, streaming_type={streaming_type}, retried={retried}/{max_retry}, start_time={start_time}, end_time={end_time}, response_type={response_type} }}",
        )
    }
}

/// Log adapter for `JobResult`. Defers to [`JobResultDataSummary`] for the
/// inner `data` body so the two summaries stay format-consistent.
pub struct JobResultSummary<'a>(pub &'a JobResult);

impl fmt::Display for JobResultSummary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if tracing::enabled!(tracing::Level::TRACE) {
            return write!(f, "{:?}", self.0);
        }
        let id = self.0.id.as_ref().map(|i| i.value);
        let meta = self.0.metadata.len();
        match self.0.data.as_ref() {
            Some(d) => write!(
                f,
                "JobResult {{ id={id:?}, metadata_keys={meta}, data={} }}",
                JobResultDataSummary(d)
            ),
            None => write!(
                f,
                "JobResult {{ id={id:?}, metadata_keys={meta}, data=None }}"
            ),
        }
    }
}

/// Log adapter for `Option<JobResult>`. Renders `None` as the literal
/// string `"None"`, otherwise delegates to [`JobResultSummary`].
pub struct OptionJobResultSummary<'a>(pub &'a Option<JobResult>);

impl fmt::Display for OptionJobResultSummary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(r) => write!(f, "{}", JobResultSummary(r)),
            None => f.write_str("None"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobworkerp::data::{JobData, JobId, WorkerId};
    use std::collections::HashMap;

    fn sample_job(args_size: usize) -> Job {
        Job {
            id: Some(JobId { value: 42 }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: 7 }),
                args: vec![0u8; args_size],
                streaming_type: 2,
                retried: 1,
                ..Default::default()
            }),
            metadata: HashMap::from([("k".to_string(), "v".to_string())]),
        }
    }

    #[test]
    fn summary_renders_byte_length_not_raw_args() {
        let job = sample_job(8 * 1024 * 1024);
        let rendered = format!("{}", JobSummary(&job));
        // Args bytes count must be visible in the summary form so
        // operators can spot oversized payloads in routine debug logs.
        assert!(
            rendered.contains("args_bytes=8388608"),
            "rendered={rendered}"
        );
        // The actual raw bytes must NOT appear: the whole point of the
        // adapter is to keep multi-MB payloads out of debug-level logs.
        // The summary is ~150 chars; the raw Debug rendering would
        // dwarf it.
        assert!(rendered.len() < 256, "rendered={rendered}");
    }

    #[test]
    fn summary_includes_job_and_worker_identifiers() {
        let job = sample_job(0);
        let rendered = format!("{}", JobSummary(&job));
        assert!(rendered.contains("id=Some(42)"), "rendered={rendered}");
        assert!(
            rendered.contains("worker_id=Some(7)"),
            "rendered={rendered}"
        );
        assert!(rendered.contains("retried=1"), "rendered={rendered}");
        assert!(rendered.contains("streaming_type=2"), "rendered={rendered}");
        assert!(rendered.contains("metadata_keys=1"), "rendered={rendered}");
    }

    #[test]
    fn summary_handles_missing_data_and_id() {
        let job = Job {
            id: None,
            data: None,
            metadata: HashMap::new(),
        };
        let rendered = format!("{}", JobSummary(&job));
        assert!(rendered.contains("id=None"), "rendered={rendered}");
        assert!(rendered.contains("args_bytes=0"), "rendered={rendered}");
        assert!(rendered.contains("metadata_keys=0"), "rendered={rendered}");
    }

    use crate::jobworkerp::data::{JobResultId, ResultOutput};

    fn sample_job_result_data(args_bytes: usize, output_bytes: usize) -> JobResultData {
        JobResultData {
            job_id: Some(JobId { value: 11 }),
            worker_id: Some(WorkerId { value: 7 }),
            worker_name: "w".to_string(),
            args: vec![0u8; args_bytes],
            status: 1,
            output: Some(ResultOutput {
                items: vec![0u8; output_bytes],
            }),
            retried: 2,
            max_retry: 5,
            streaming_type: 3,
            start_time: 100,
            end_time: 200,
            response_type: 4,
            ..Default::default()
        }
    }

    #[test]
    fn job_result_data_summary_renders_byte_lengths_not_raw_payloads() {
        let d = sample_job_result_data(8 * 1024 * 1024, 4 * 1024 * 1024);
        let rendered = format!("{}", JobResultDataSummary(&d));
        assert!(
            rendered.contains("args_bytes=8388608"),
            "rendered={rendered}"
        );
        assert!(
            rendered.contains("output_bytes=4194304"),
            "rendered={rendered}"
        );
        assert!(rendered.len() < 256, "rendered={rendered}");
    }

    #[test]
    fn job_result_data_summary_includes_identifiers() {
        let d = sample_job_result_data(0, 0);
        let rendered = format!("{}", JobResultDataSummary(&d));
        assert!(rendered.contains("job_id=Some(11)"), "rendered={rendered}");
        assert!(
            rendered.contains("worker_id=Some(7)"),
            "rendered={rendered}"
        );
        assert!(
            rendered.contains(r#"worker_name="w""#),
            "rendered={rendered}"
        );
        assert!(rendered.contains("status=1"), "rendered={rendered}");
        assert!(rendered.contains("retried=2/5"), "rendered={rendered}");
        assert!(rendered.contains("streaming_type=3"), "rendered={rendered}");
        assert!(rendered.contains("start_time=100"), "rendered={rendered}");
        assert!(rendered.contains("end_time=200"), "rendered={rendered}");
        assert!(rendered.contains("response_type=4"), "rendered={rendered}");
    }

    #[test]
    fn job_result_data_summary_handles_missing_optionals() {
        let d = JobResultData::default();
        let rendered = format!("{}", JobResultDataSummary(&d));
        assert!(rendered.contains("job_id=None"), "rendered={rendered}");
        assert!(rendered.contains("worker_id=None"), "rendered={rendered}");
        assert!(rendered.contains("args_bytes=0"), "rendered={rendered}");
        assert!(rendered.contains("output_bytes=0"), "rendered={rendered}");
    }

    #[test]
    fn job_result_summary_delegates_to_data_summary() {
        let jr = JobResult {
            id: Some(JobResultId { value: 99 }),
            data: Some(sample_job_result_data(1024, 0)),
            metadata: HashMap::from([("k".to_string(), "v".to_string())]),
        };
        let rendered = format!("{}", JobResultSummary(&jr));
        assert!(rendered.contains("id=Some(99)"), "rendered={rendered}");
        assert!(rendered.contains("metadata_keys=1"), "rendered={rendered}");
        // Nested data body should render via JobResultDataSummary.
        assert!(
            rendered.contains("data=JobResultData {"),
            "rendered={rendered}"
        );
        assert!(rendered.contains("args_bytes=1024"), "rendered={rendered}");
    }

    #[test]
    fn job_result_summary_handles_missing_data_and_id() {
        let jr = JobResult {
            id: None,
            data: None,
            metadata: HashMap::new(),
        };
        let rendered = format!("{}", JobResultSummary(&jr));
        assert!(rendered.contains("id=None"), "rendered={rendered}");
        assert!(rendered.contains("metadata_keys=0"), "rendered={rendered}");
        assert!(rendered.contains("data=None"), "rendered={rendered}");
    }

    #[test]
    fn option_job_result_summary_renders_none_and_some() {
        let none: Option<JobResult> = None;
        assert_eq!(format!("{}", OptionJobResultSummary(&none)), "None");

        let jr = JobResult {
            id: Some(JobResultId { value: 1 }),
            data: None,
            metadata: HashMap::new(),
        };
        let some = Some(jr.clone());
        assert_eq!(
            format!("{}", OptionJobResultSummary(&some)),
            format!("{}", JobResultSummary(&jr))
        );
    }
}
