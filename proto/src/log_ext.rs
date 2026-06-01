use crate::jobworkerp::data::Job;
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
        // Coupling `Display` to the tracing level is unusual but
        // intentional: it keeps the two rendering paths next to each
        // other and removes branching noise at every callsite.
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
}
