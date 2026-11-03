use super::*;
use crate::workflow::position::WorkflowPosition;
use once_cell::sync::Lazy;

/// Macro to reduce repetitive error handling with workflow position context
///
/// This macro simplifies error handling by automatically capturing the workflow position
/// and creating appropriate error instances. It's particularly useful in async contexts
/// where task execution needs to track position information.
///
/// Stack traces are logged via tracing::error for debugging purposes,
/// while user-facing error messages use Display format for readability.
///
/// # Usage
///
/// Basic usage with error type and message:
/// ```ignore
/// let result = bail_with_position!(
///     task_context,
///     some_operation(),
///     runtime,
///     "Operation failed"
/// );
/// ```
///
/// With custom detail message:
/// ```ignore
/// let result = bail_with_position!(
///     task_context,
///     some_operation(),
///     runtime,
///     "Operation failed",
///     "Additional detail information"
/// );
/// ```
///
/// # Parameters
///
/// - `$task_context`: A reference to the task context containing position information
/// - `$result`: The Result to unwrap or convert to workflow error
/// - `$error_type`: The error factory method name (e.g., runtime, configuration)
/// - `$message`: The error message to include in the error
/// - `$detail` (optional): Additional detail message (defaults to Display representation of error)
#[macro_export]
macro_rules! bail_with_position {
    ($task_context:expr, $result:expr, $error_type:ident, $message:expr) => {
        match $result {
            Ok(val) => val,
            Err(e) => {
                let pos = $task_context.position.read().await;
                tracing::error!(
                    error = ?e,
                    position = %pos.as_error_instance(),
                    "{}", $message
                );
                return Err(
                    $crate::workflow::definition::workflow::errors::ErrorFactory::new()
                        .$error_type(
                            $message.to_string(),
                            Some(pos.as_error_instance()),
                            Some(e.to_string()),
                        ),
                );
            }
        }
    };
    ($task_context:expr, $result:expr, $error_type:ident, $message:expr, $detail:expr) => {
        match $result {
            Ok(val) => val,
            Err(e) => {
                let pos = $task_context.position.read().await;
                tracing::error!(
                    error = ?e,
                    position = %pos.as_error_instance(),
                    detail = %$detail,
                    "{}", $message
                );
                return Err(
                    $crate::workflow::definition::workflow::errors::ErrorFactory::new()
                        .$error_type(
                            $message.to_string(),
                            Some(pos.as_error_instance()),
                            Some($detail),
                        ),
                );
            }
        }
    };
}

impl super::Error {
    pub fn position(&mut self, position: &WorkflowPosition) -> &mut Self {
        self.instance = Some(position.as_error_instance());
        self
    }
    pub fn message(&mut self, message: &str) -> &mut Self {
        self.title = Some(message.into());
        self
    }
}
impl std::error::Error for super::Error {}

impl Default for Error {
    fn default() -> Self {
        Self {
            detail: Default::default(),
            instance: Default::default(),
            status: Default::default(),
            title: Default::default(),
            type_: super::UriTemplate("default".to_string()),
        }
    }
}

impl std::fmt::Display for super::Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Error: {} {} {}",
            self.title.as_deref().unwrap_or("Unknown error"),
            self.detail.as_deref().unwrap_or("No details"),
            self.instance.as_deref().unwrap_or("No instance"),
        )
    }
}
impl UriTemplate {
    /// Whether the concrete `uri` matches this template; each `{var}` matches
    /// any text. Comparison is case-sensitive.
    pub fn is_match(&self, uri: &str) -> bool {
        let template_str = &self.0;
        let template_parts: Vec<&str> = template_str.split('{').collect();
        let mut pattern = String::new();

        for (i, part) in template_parts.iter().enumerate() {
            if i == 0 {
                pattern.push_str(&regex::escape(part));
                continue;
            }

            if let Some(idx) = part.find('}') {
                pattern.push_str("(.*)");
                pattern.push_str(&regex::escape(&part[idx + 1..]));
            } else {
                pattern.push_str(&regex::escape(&format!("{{{part}")));
            }
        }

        match regex::Regex::new(&format!("^{pattern}$")) {
            Ok(re) => re.is_match(uri),
            Err(_) => false,
        }
    }
}

/// Base of the standard error types. Version 1.0.0 of the specification was
/// published under the former Serverless Workflow domain, so both spellings
/// identify the same error type.
macro_rules! spec_uri_base {
    () => {
        "https://open-workflow-specification.org/spec/"
    };
}
const SPEC_URI_BASE: &str = spec_uri_base!();
const LEGACY_SPEC_URI_BASE: &str = "https://serverlessworkflow.io/spec/";

fn canonical_spec_uri(uri: &str) -> std::borrow::Cow<'_, str> {
    match uri.strip_prefix(LEGACY_SPEC_URI_BASE) {
        Some(rest) => std::borrow::Cow::Owned(format!("{SPEC_URI_BASE}{rest}")),
        None => std::borrow::Cow::Borrowed(uri),
    }
}

/// Whether `value` has the shape the workflow schema requires of a literal
/// error type (`$defs/uriTemplate`): a scheme followed by `://`.
fn is_absolute_uri(value: &str) -> bool {
    static PATTERN: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(r"^[A-Za-z][A-Za-z0-9+\-.]*://").expect("valid absolute URI pattern")
    });
    PATTERN.is_match(value)
}

impl ErrorFilter {
    /// Whether `error` satisfies every field set on this filter; unset fields
    /// match anything.
    pub fn matches(&self, error: &Error) -> bool {
        // The type check builds a regex, so it runs last.
        let matched = self.status.is_none_or(|st| st == error.status)
            && Self::field_matches(self.details.as_deref(), &error.detail)
            && Self::field_matches(self.title.as_deref(), &error.title)
            && Self::field_matches(self.instance.as_deref(), &error.instance)
            && self
                .type_
                .as_deref()
                .is_none_or(|filter_type| Self::type_matches(filter_type, &error.type_.0));
        tracing::debug!(matched, filter = ?self, "Evaluated catch error filter");
        matched
    }

    /// The filter type is the template; the error type is the concrete value.
    fn type_matches(filter_type: &str, error_type: &str) -> bool {
        if !is_absolute_uri(filter_type) {
            // Literal error types must be absolute URIs, so a scheme-less filter
            // usually is a leftover from an older, non-URI error type format.
            tracing::warn!(
                filter_type,
                "catch.errors.with.type is not an absolute URI; it is matched as a literal string"
            );
        }
        UriTemplate(canonical_spec_uri(filter_type).into_owned())
            .is_match(&canonical_spec_uri(error_type))
    }

    fn field_matches(expected: Option<&str>, actual: &Option<String>) -> bool {
        expected.is_none_or(|expected| actual.as_deref() == Some(expected))
    }
}

macro_rules! spec_error_type {
    ($kind:literal) => {
        concat!(spec_uri_base!(), "1.0.0/errors/", $kind)
    };
}

/// The standard error categories defined by the Open Workflow Specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardErrorType {
    Configuration,
    Validation,
    Expression,
    Authentication,
    Authorization,
    Timeout,
    Communication,
    Runtime,
}

impl StandardErrorType {
    pub const fn uri(self) -> &'static str {
        match self {
            Self::Configuration => spec_error_type!("configuration"),
            Self::Validation => spec_error_type!("validation"),
            Self::Expression => spec_error_type!("expression"),
            Self::Authentication => spec_error_type!("authentication"),
            Self::Authorization => spec_error_type!("authorization"),
            Self::Timeout => spec_error_type!("timeout"),
            Self::Communication => spec_error_type!("communication"),
            Self::Runtime => spec_error_type!("runtime"),
        }
    }

    const ALL: [Self; 8] = [
        Self::Configuration,
        Self::Validation,
        Self::Expression,
        Self::Authentication,
        Self::Authorization,
        Self::Timeout,
        Self::Communication,
        Self::Runtime,
    ];

    /// The category whose URI `uri` is, accepting the legacy spec domain.
    pub fn from_uri(uri: &str) -> Option<Self> {
        let uri = canonical_spec_uri(uri);
        Self::ALL.into_iter().find(|kind| kind.uri() == uri)
    }

    /// The status the specification assigns to this category.
    pub const fn default_status(self) -> i64 {
        match self {
            Self::Configuration | Self::Validation | Self::Expression => 400,
            Self::Authentication => 401,
            Self::Authorization => 403,
            Self::Timeout => 408,
            Self::Communication | Self::Runtime => 500,
        }
    }
}

pub const ERROR_TYPE_RUNTIME: &str = StandardErrorType::Runtime.uri();
pub const ERROR_TYPE_TIMEOUT: &str = StandardErrorType::Timeout.uri();

#[derive(Debug, Clone, Default)]
pub struct ErrorFactory {}

impl ErrorFactory {
    pub fn new() -> Self {
        Self {}
    }

    /// Builds a standard error with the category's URI and default status.
    pub fn standard(
        &self,
        kind: StandardErrorType,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<Error> {
        self.standard_with_status(kind, kind.default_status(), message, position, detail)
    }

    /// Builds a standard error that carries the status reported by the origin
    /// (e.g. a downstream HTTP response) instead of the category default.
    pub fn standard_with_status(
        &self,
        kind: StandardErrorType,
        status: i64,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<Error> {
        Box::new(Error {
            type_: UriTemplate(kind.uri().to_string()),
            status,
            title: Some(message),
            detail,
            instance: position,
        })
    }

    // No `authentication` constructor: the engine raises no authentication
    // failures yet; use `standard(StandardErrorType::Authentication, ..)`.

    /// Invalid or unsupported workflow definition or deployment setup.
    pub fn configuration(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<Error> {
        self.standard(StandardErrorType::Configuration, message, position, detail)
    }

    /// Input or output data failed schema validation.
    pub fn validation(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<Error> {
        self.standard(StandardErrorType::Validation, message, position, detail)
    }

    /// A runtime expression failed to evaluate.
    pub fn expression(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<Error> {
        self.standard(StandardErrorType::Expression, message, position, detail)
    }

    /// Access to a secret that is not declared or not available.
    pub fn authorization(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<Error> {
        self.standard(StandardErrorType::Authorization, message, position, detail)
    }

    /// A task or workflow exceeded its time limit.
    pub fn timeout(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<Error> {
        self.standard(StandardErrorType::Timeout, message, position, detail)
    }

    /// A call to an external service failed.
    pub fn communication(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<Error> {
        self.standard(StandardErrorType::Communication, message, position, detail)
    }

    /// Any other failure while executing the workflow.
    pub fn runtime(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<Error> {
        self.standard(StandardErrorType::Runtime, message, position, detail)
    }
}

#[cfg(test)]
mod spec_error_type_tests {
    use super::*;

    #[test]
    fn timeout_error_uses_spec_type_and_408() {
        let err = ErrorFactory::new().timeout("t".to_string(), Some("/do/0".to_string()), None);
        assert_eq!(err.type_.0, ERROR_TYPE_TIMEOUT);
        assert_eq!(err.status, 408);
        assert_eq!(err.title.as_deref(), Some("t"));
        assert_eq!(err.instance.as_deref(), Some("/do/0"));
    }

    const ALL_STANDARD_TYPES: [(StandardErrorType, &str, i64); 8] = [
        (StandardErrorType::Configuration, "configuration", 400),
        (StandardErrorType::Validation, "validation", 400),
        (StandardErrorType::Expression, "expression", 400),
        (StandardErrorType::Authentication, "authentication", 401),
        (StandardErrorType::Authorization, "authorization", 403),
        (StandardErrorType::Timeout, "timeout", 408),
        (StandardErrorType::Communication, "communication", 500),
        (StandardErrorType::Runtime, "runtime", 500),
    ];

    #[test]
    fn from_uri_recognizes_every_standard_type_in_both_spec_domains() {
        // The lookup table and this test table must list the same categories.
        assert_eq!(
            StandardErrorType::ALL,
            ALL_STANDARD_TYPES.map(|(kind, _, _)| kind)
        );
        for (kind, name, _) in ALL_STANDARD_TYPES {
            assert_eq!(
                StandardErrorType::from_uri(kind.uri()),
                Some(kind),
                "{name}"
            );
            let legacy = format!("https://serverlessworkflow.io/spec/1.0.0/errors/{name}");
            assert_eq!(StandardErrorType::from_uri(&legacy), Some(kind), "{legacy}");
        }
    }

    #[test]
    fn from_uri_rejects_other_types() {
        for other in [
            "https://example.com/errors/custom",
            "https://open-workflow-specification.org/spec/1.0.0/errors/Timeout",
            "https://open-workflow-specification.org/spec/1.0.0/errors/timeout/extra",
            "timeout",
            "",
        ] {
            assert_eq!(StandardErrorType::from_uri(other), None, "{other}");
        }
    }

    #[test]
    fn standard_with_status_keeps_type_and_overrides_only_status() {
        let err = ErrorFactory::new().standard_with_status(
            StandardErrorType::Communication,
            404,
            "HTTP 404 Not Found".to_string(),
            Some("/do/0/fetch".to_string()),
            Some("missing".to_string()),
        );
        assert_eq!(err.type_.0, StandardErrorType::Communication.uri());
        assert_eq!(err.status, 404);
        assert_eq!(err.title.as_deref(), Some("HTTP 404 Not Found"));
        assert_eq!(err.instance.as_deref(), Some("/do/0/fetch"));
        assert_eq!(err.detail.as_deref(), Some("missing"));
    }

    #[test]
    fn standard_error_carries_spec_type_default_status_and_fields() {
        for (kind, name, status) in ALL_STANDARD_TYPES {
            let err = ErrorFactory::new().standard(
                kind,
                "title".to_string(),
                Some("/do/0".to_string()),
                Some("detail".to_string()),
            );
            assert_eq!(
                err.type_.0,
                format!("https://open-workflow-specification.org/spec/1.0.0/errors/{name}")
            );
            assert_eq!(err.status, status, "{name}");
            assert_eq!(err.title.as_deref(), Some("title"));
            assert_eq!(err.instance.as_deref(), Some("/do/0"));
            assert_eq!(err.detail.as_deref(), Some("detail"));
        }
    }

    #[test]
    fn named_constructors_build_the_matching_standard_error() {
        type Constructor = fn(&ErrorFactory, String, Option<String>, Option<String>) -> Box<Error>;
        let constructors: [(Constructor, StandardErrorType); 7] = [
            (
                ErrorFactory::configuration,
                StandardErrorType::Configuration,
            ),
            (ErrorFactory::validation, StandardErrorType::Validation),
            (ErrorFactory::expression, StandardErrorType::Expression),
            (
                ErrorFactory::authorization,
                StandardErrorType::Authorization,
            ),
            (ErrorFactory::timeout, StandardErrorType::Timeout),
            (
                ErrorFactory::communication,
                StandardErrorType::Communication,
            ),
            (ErrorFactory::runtime, StandardErrorType::Runtime),
        ];
        let factory = ErrorFactory::new();
        for (constructor, kind) in constructors {
            let built = constructor(&factory, "m".to_string(), Some("/p".to_string()), None);
            let expected = factory.standard(kind, "m".to_string(), Some("/p".to_string()), None);
            assert_eq!(built, expected, "{kind:?}");
        }
    }

    #[test]
    fn standard_errors_match_spec_filters_but_not_legacy_ones() {
        let timeout = ErrorFactory::new().timeout("t".to_string(), None, None);
        assert!(type_filter(ERROR_TYPE_TIMEOUT).matches(&timeout));
        let runtime = error_of_type(ERROR_TYPE_RUNTIME);
        for (legacy, err) in [
            ("http-error://requesttimeout", &*timeout),
            ("requesttimeout", &*timeout),
            ("http-error://internalerror", &runtime),
            ("internalerror", &runtime),
        ] {
            assert!(!type_filter(legacy).matches(err), "{legacy} must not match");
        }
    }

    fn type_filter(type_: &str) -> ErrorFilter {
        ErrorFilter {
            type_: Some(type_.to_string()),
            ..Default::default()
        }
    }

    fn error_of_type(type_: &str) -> Error {
        Error {
            type_: UriTemplate(type_.to_string()),
            status: 500,
            title: Some("title".to_string()),
            detail: Some("detail".to_string()),
            instance: Some("/do/0".to_string()),
        }
    }

    const STANDARD_KINDS: [&str; 8] = [
        "configuration",
        "validation",
        "expression",
        "authentication",
        "authorization",
        "timeout",
        "communication",
        "runtime",
    ];

    #[test]
    fn type_filter_matches_identical_type_case_sensitively() {
        let err = error_of_type("https://example.invalid/Errors/Upstream");
        assert!(type_filter("https://example.invalid/Errors/Upstream").matches(&err));
        assert!(!type_filter("https://example.invalid/errors/upstream").matches(&err));
    }

    #[test]
    fn type_filter_template_matches_every_standard_error() {
        let filter =
            type_filter("https://open-workflow-specification.org/spec/1.0.0/errors/{kind}");
        for kind in STANDARD_KINDS {
            let err = error_of_type(&format!(
                "https://open-workflow-specification.org/spec/1.0.0/errors/{kind}"
            ));
            assert!(filter.matches(&err), "{kind} must match the template");
        }
        assert!(!filter.matches(&error_of_type("https://example.invalid/errors/runtime")));
    }

    #[test]
    fn type_filter_is_the_template_not_the_error_type() {
        // A `{...}` in the error type must not act as a wildcard.
        let err = error_of_type("https://example.invalid/errors/{kind}");
        assert!(!type_filter("https://example.invalid/errors/runtime").matches(&err));
    }

    #[test]
    fn scheme_less_filter_still_matches_literally() {
        // Raise errors built from expressions can carry any string type.
        assert!(type_filter("task-error").matches(&error_of_type("task-error")));
    }

    #[test]
    fn custom_scheme_filter_matches_the_same_custom_type() {
        let err = error_of_type("http-error://foo");
        assert!(type_filter("http-error://foo").matches(&err));
    }

    #[test]
    fn legacy_spec_domain_is_equivalent_in_both_directions() {
        let legacy = "https://serverlessworkflow.io/spec/1.0.0/errors/runtime";
        assert!(type_filter(legacy).matches(&error_of_type(ERROR_TYPE_RUNTIME)));
        assert!(type_filter(ERROR_TYPE_RUNTIME).matches(&error_of_type(legacy)));
        assert!(
            !type_filter("https://serverlessworkflow.io/spec/2.0.0/errors/runtime")
                .matches(&error_of_type(ERROR_TYPE_RUNTIME))
        );
    }

    #[test]
    fn absolute_uri_detection_follows_the_spec_pattern() {
        for valid in ["https://example.invalid/x", "http-error://foo", "urn+x://y"] {
            assert!(is_absolute_uri(valid), "{valid} has a scheme");
        }
        for invalid in ["badargument", "", "://x", "1abc://x", "urn:x"] {
            assert!(!is_absolute_uri(invalid), "{invalid} has no scheme");
        }
    }

    #[test]
    fn non_type_fields_are_combined_with_and() {
        let err = error_of_type(ERROR_TYPE_RUNTIME);
        let filter = ErrorFilter {
            type_: Some(ERROR_TYPE_RUNTIME.to_string()),
            status: Some(500),
            title: Some("title".to_string()),
            details: Some("detail".to_string()),
            instance: Some("/do/0".to_string()),
        };
        assert!(filter.matches(&err));
        for mismatch in [
            ErrorFilter {
                status: Some(400),
                ..filter.clone()
            },
            ErrorFilter {
                title: Some("other".to_string()),
                ..filter.clone()
            },
            ErrorFilter {
                details: Some("other".to_string()),
                ..filter.clone()
            },
            ErrorFilter {
                instance: Some("/do/1".to_string()),
                ..filter.clone()
            },
        ] {
            assert!(!mismatch.matches(&err), "{mismatch:?} must not match");
        }
    }
}
