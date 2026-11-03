//! Conversion between workflow errors and the classification a failed job
//! reports (`StreamError`), in both directions: a faulted WORKFLOW job encodes
//! its workflow error, and the workflow that ran the job raises it again.

use crate::workflow::definition::workflow::{self, errors::StandardErrorType};
use proto::jobworkerp::data::{ProblemDetails, RunnerType, StreamError, StreamErrorCode};

/// Standard error type for a reported classification code. Execution
/// failures, cancellations and unknown codes have no matching type.
pub(crate) fn kind_for_code(code: i32) -> Option<StandardErrorType> {
    match StreamErrorCode::try_from(code).ok()? {
        StreamErrorCode::Timeout => Some(StandardErrorType::Timeout),
        StreamErrorCode::TransportFailed | StreamErrorCode::ProviderStreamFailed => {
            Some(StandardErrorType::Communication)
        }
        StreamErrorCode::InvalidArgument => Some(StandardErrorType::Validation),
        StreamErrorCode::Unspecified
        | StreamErrorCode::ExecutionFailed
        | StreamErrorCode::Cancelled => None,
    }
}

/// Classification code for a workflow error type, the inverse of
/// [`kind_for_code`] for the types it covers.
fn code_for_type(type_uri: &str) -> StreamErrorCode {
    match StandardErrorType::from_uri(type_uri) {
        Some(StandardErrorType::Timeout) => StreamErrorCode::Timeout,
        Some(StandardErrorType::Communication) => StreamErrorCode::TransportFailed,
        Some(StandardErrorType::Validation) => StreamErrorCode::InvalidArgument,
        _ => StreamErrorCode::ExecutionFailed,
    }
}

/// Classification of a failed WORKFLOW job: the code gives the coarse cause
/// for readers that ignore `problem`, and `problem` carries the workflow error.
pub(crate) fn workflow_failure(error: &workflow::Error, diagnostic: &str) -> StreamError {
    let mut failure = proto::stream_error::classified_error(
        code_for_type(&error.type_.0),
        diagnostic,
        RunnerType::Workflow.as_str_name(),
    );
    failure.problem = Some(ProblemDetails {
        r#type: error.type_.0.clone(),
        status: i32::try_from(error.status).unwrap_or_default(),
        title: error.title.clone(),
        detail: error.detail.clone(),
        instance: error.instance.clone(),
    });
    failure
}

/// The error a failed job reported, raised at `position` in the workflow that
/// ran it. The reported instance points into the job's own workflow, so it
/// moves to the detail. `None` when the problem has no type to raise.
pub(crate) fn raise_reported(
    problem: &ProblemDetails,
    position: Option<String>,
) -> Option<Box<workflow::Error>> {
    if problem.r#type.is_empty() {
        return None;
    }
    // The workflow schema requires a status, so 0 only comes from producers
    // that did not set one.
    let status = match problem.status {
        status if status > 0 => i64::from(status),
        _ => StandardErrorType::from_uri(&problem.r#type)
            .unwrap_or(StandardErrorType::Runtime)
            .default_status(),
    };
    let detail = match (&problem.detail, &problem.instance) {
        (Some(detail), Some(instance)) => Some(format!(
            "{detail} (raised at {instance} in the child workflow)"
        )),
        (None, Some(instance)) => Some(format!("raised at {instance} in the child workflow")),
        (detail, None) => detail.clone(),
    };
    Some(Box::new(workflow::Error {
        type_: workflow::UriTemplate(problem.r#type.clone()),
        status,
        title: problem.title.clone(),
        detail,
        instance: position,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUSTOM_TYPE: &str = "https://example.com/errors/custom";

    fn child_error(type_uri: &str, status: i64) -> workflow::Error {
        workflow::Error {
            type_: workflow::UriTemplate(type_uri.to_string()),
            status,
            title: Some("Child failed".to_string()),
            detail: Some("child detail".to_string()),
            instance: Some("/do/0/inner".to_string()),
        }
    }

    #[test]
    fn codes_and_types_map_back_to_each_other() {
        use StandardErrorType::*;
        for kind in [
            Configuration,
            Validation,
            Expression,
            Authentication,
            Authorization,
            Timeout,
            Communication,
            Runtime,
        ] {
            let code = code_for_type(kind.uri());
            match kind_for_code(code as i32) {
                Some(back) => assert_eq!(back, kind, "{kind:?}"),
                None => assert_eq!(code, StreamErrorCode::ExecutionFailed, "{kind:?}"),
            }
        }
        assert_eq!(code_for_type(CUSTOM_TYPE), StreamErrorCode::ExecutionFailed);
        assert_eq!(kind_for_code(99), None);
    }

    #[test]
    fn workflow_failure_carries_the_error_as_a_problem_and_a_code() {
        let cases = [
            (StandardErrorType::Timeout.uri(), StreamErrorCode::Timeout),
            (
                StandardErrorType::Communication.uri(),
                StreamErrorCode::TransportFailed,
            ),
            (
                StandardErrorType::Validation.uri(),
                StreamErrorCode::InvalidArgument,
            ),
            (
                StandardErrorType::Runtime.uri(),
                StreamErrorCode::ExecutionFailed,
            ),
            (CUSTOM_TYPE, StreamErrorCode::ExecutionFailed),
        ];
        for (type_uri, code) in cases {
            let failure = workflow_failure(&child_error(type_uri, 418), "diagnostic");
            assert_eq!(failure.code, code as i32, "{type_uri}");
            assert_eq!(failure.origin, "WORKFLOW");
            assert_eq!(failure.message, "diagnostic");
            let problem = failure.problem.expect("problem");
            assert_eq!(problem.r#type, type_uri);
            assert_eq!(problem.status, 418);
            assert_eq!(problem.title.as_deref(), Some("Child failed"));
            assert_eq!(problem.detail.as_deref(), Some("child detail"));
            assert_eq!(problem.instance.as_deref(), Some("/do/0/inner"));
        }
    }

    fn problem_of(error: &workflow::Error) -> ProblemDetails {
        workflow_failure(error, "diagnostic").problem.unwrap()
    }

    #[test]
    fn reported_error_is_raised_at_the_given_position() {
        let raised = raise_reported(
            &problem_of(&child_error(CUSTOM_TYPE, 418)),
            Some("/do/1".into()),
        )
        .unwrap();
        assert_eq!(raised.type_.0, CUSTOM_TYPE);
        assert_eq!(raised.status, 418);
        assert_eq!(raised.title.as_deref(), Some("Child failed"));
        assert_eq!(raised.instance.as_deref(), Some("/do/1"));
        assert_eq!(
            raised.detail.as_deref(),
            Some("child detail (raised at /do/0/inner in the child workflow)")
        );
    }

    #[test]
    fn missing_status_and_detail_are_filled_in() {
        let mut timeout = child_error(StandardErrorType::Timeout.uri(), 0);
        timeout.detail = None;
        let raised = raise_reported(&problem_of(&timeout), None).unwrap();
        assert_eq!(raised.status, 408);
        assert_eq!(
            raised.detail.as_deref(),
            Some("raised at /do/0/inner in the child workflow")
        );

        let mut custom = child_error(CUSTOM_TYPE, 0);
        custom.instance = None;
        let raised = raise_reported(&problem_of(&custom), None).unwrap();
        assert_eq!(raised.status, 500);
        assert_eq!(raised.detail.as_deref(), Some("child detail"));
    }

    #[test]
    fn problem_without_a_type_is_not_raised() {
        let mut problem = problem_of(&child_error(CUSTOM_TYPE, 418));
        problem.r#type.clear();
        assert!(raise_reported(&problem, None).is_none());
    }
}
