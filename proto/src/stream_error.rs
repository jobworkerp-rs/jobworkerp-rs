//! Shared contract for failures that terminate a result stream.
//!
//! The authoritative signal is `Trailer.error`. While older readers are still
//! deployed, error Ends also carry the v1 JSON value under
//! [`STREAM_ERROR_METADATA_KEY`] (dual write), and readers keep accepting the
//! JSON value and the legacy plugin key so Ends that crossed an older component
//! (which drops unknown protobuf fields) are still recognized as failures.

use std::{collections::HashMap, error::Error, fmt};

use serde::Serialize;
use serde_json::Value;

use crate::jobworkerp::data::{
    ResultOutputItem, ResultStatus, StreamError, StreamErrorCode, Trailer, result_output_item,
};

/// Reserved Trailer metadata key carrying the deprecated v1 JSON stream error.
pub const STREAM_ERROR_METADATA_KEY: &str = "stream_error";

/// Reserved Trailer metadata key used by V2 plugins and idle timeouts before
/// `Trailer.error` existed. Deprecated: kept for compatibility only.
pub const LEGACY_PLUGIN_STREAM_ERROR_KEY: &str = "jobworkerp.plugin.run_stream_error";

/// Maximum size of the v1 JSON value (and therefore of the message), in UTF-8 bytes.
pub const MAX_STREAM_ERROR_BYTES: usize = 4096;

/// Maximum size of `StreamError.origin`, in UTF-8 bytes.
pub const MAX_STREAM_ERROR_ORIGIN_BYTES: usize = 256;

/// Origin for failures detected outside a runner (worker / app layer).
pub const WORKER_ORIGIN: &str = "WORKER";

/// Origin substituted when a producer passes an empty origin.
pub const UNKNOWN_ORIGIN: &str = "UNKNOWN";

/// Origin reported for an End that only carries the legacy plugin key.
pub const LEGACY_PLUGIN_ORIGIN: &str = "PLUGIN";

const CODE_NAME_PREFIX: &str = "STREAM_ERROR_CODE_";
const RESERVED_KEYS: [&str; 2] = [STREAM_ERROR_METADATA_KEY, LEGACY_PLUGIN_STREAM_ERROR_KEY];

/// The result of inspecting a stream's End Trailer.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamErrorOutcome {
    Success,
    Failed(StreamError),
}

/// `anyhow`-compatible error for a failed End, so callers can downcast to read the code.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamFailedError(pub StreamError);

impl fmt::Display for StreamFailedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&describe(&self.0))
    }
}

impl Error for StreamFailedError {}

/// Builds an End Trailer that reports a failure.
///
/// Never fails: invalid fields are normalized (see the stream error contract)
/// so a producer can always terminate the stream with a failure.
pub fn error_trailer(
    mut metadata: HashMap<String, String>,
    code: StreamErrorCode,
    message: impl Into<String>,
    origin: impl Into<String>,
) -> Trailer {
    debug_assert!(
        code != StreamErrorCode::Unspecified,
        "producers must classify stream errors"
    );
    strip_reserved_keys(&mut metadata);

    let origin = normalize_origin(origin.into());
    let code_str = code_to_json_str(code as i32);
    let (json, message) = encode_bounded_v1(code_str, message.into(), &origin);
    metadata.insert(STREAM_ERROR_METADATA_KEY.to_string(), json);

    Trailer {
        metadata,
        error: Some(StreamError {
            code: code as i32,
            message,
            origin,
        }),
    }
}

/// [`error_trailer`] wrapped in an End item.
pub fn error_end_item(
    metadata: HashMap<String, String>,
    code: StreamErrorCode,
    message: impl Into<String>,
    origin: impl Into<String>,
) -> ResultOutputItem {
    end_item(error_trailer(metadata, code, message, origin))
}

/// [`error_trailer`] that also writes the deprecated legacy plugin key, for
/// producers whose older readers only understand that key.
pub fn error_trailer_with_legacy_key(
    metadata: HashMap<String, String>,
    code: StreamErrorCode,
    message: impl Into<String>,
    origin: impl Into<String>,
) -> Trailer {
    let mut trailer = error_trailer(metadata, code, message, origin);
    // Reuse the bounded message so every representation carries the same text.
    let message = trailer
        .error
        .as_ref()
        .expect("error_trailer always sets the error")
        .message
        .clone();
    trailer
        .metadata
        .insert(LEGACY_PLUGIN_STREAM_ERROR_KEY.to_string(), message);
    trailer
}

/// [`error_trailer_with_legacy_key`] wrapped in an End item.
pub fn error_end_item_with_legacy_key(
    metadata: HashMap<String, String>,
    code: StreamErrorCode,
    message: impl Into<String>,
    origin: impl Into<String>,
) -> ResultOutputItem {
    end_item(error_trailer_with_legacy_key(
        metadata, code, message, origin,
    ))
}

/// Builds an End Trailer for a successful execution.
///
/// Reserved keys are removed so caller-controlled metadata (e.g. request
/// metadata echoed into the Trailer) cannot make a success look like a failure.
pub fn normal_trailer(mut metadata: HashMap<String, String>) -> Trailer {
    strip_reserved_keys(&mut metadata);
    Trailer {
        metadata,
        error: None,
    }
}

/// [`normal_trailer`] wrapped in an End item.
pub fn normal_end_item(metadata: HashMap<String, String>) -> ResultOutputItem {
    end_item(normal_trailer(metadata))
}

/// Wraps a Trailer in an End item.
pub fn end_item(trailer: Trailer) -> ResultOutputItem {
    ResultOutputItem {
        item: Some(result_output_item::Item::End(trailer)),
    }
}

/// Decides whether an End reports success, reading the typed field first and
/// the compatibility keys after it. Malformed compatibility values are failures.
pub fn inspect_end(trailer: &Trailer) -> StreamErrorOutcome {
    if let Some(error) = &trailer.error {
        return StreamErrorOutcome::Failed(error.clone());
    }
    if let Some(encoded) = trailer.metadata.get(STREAM_ERROR_METADATA_KEY) {
        let error = match parse_v1(encoded) {
            Ok(error) => error,
            Err(reason) => StreamError {
                code: StreamErrorCode::Unspecified as i32,
                message: format!("malformed {STREAM_ERROR_METADATA_KEY} metadata: {reason:?}"),
                origin: UNKNOWN_ORIGIN.to_string(),
            },
        };
        return StreamErrorOutcome::Failed(error);
    }
    if let Some(message) = trailer.metadata.get(LEGACY_PLUGIN_STREAM_ERROR_KEY) {
        return StreamErrorOutcome::Failed(StreamError {
            code: StreamErrorCode::ExecutionFailed as i32,
            message: message.clone(),
            origin: LEGACY_PLUGIN_ORIGIN.to_string(),
        });
    }
    StreamErrorOutcome::Success
}

/// The failure reported by an End, if any.
pub fn end_failure(trailer: &Trailer) -> Option<StreamError> {
    match inspect_end(trailer) {
        StreamErrorOutcome::Success => None,
        StreamErrorOutcome::Failed(error) => Some(error),
    }
}

/// [`inspect_end`] as an `anyhow::Result`; the error downcasts to [`StreamFailedError`].
pub fn ensure_end_success(trailer: &Trailer) -> anyhow::Result<()> {
    match inspect_end(trailer) {
        StreamErrorOutcome::Success => Ok(()),
        StreamErrorOutcome::Failed(error) => Err(StreamFailedError(error).into()),
    }
}

/// Returns the End metadata of a successful stream, or the stream failure.
pub fn success_metadata(trailer: Trailer) -> anyhow::Result<HashMap<String, String>> {
    ensure_end_success(&trailer)?;
    Ok(trailer.metadata)
}

/// Maps a stream error code to the terminal `ResultStatus` reported to consumers.
///
/// Retrying is impossible once stream data reached the consumer, so retryable
/// classes resolve to the exhausted-retry status.
pub fn result_status_for(code: i32) -> ResultStatus {
    match StreamErrorCode::try_from(code) {
        Ok(StreamErrorCode::Cancelled) => ResultStatus::Cancelled,
        Ok(StreamErrorCode::InvalidArgument) => ResultStatus::OtherError,
        _ => ResultStatus::MaxRetry,
    }
}

/// Code name used by the v1 JSON contract (enum name without the prefix).
/// Unknown values map to `UNSPECIFIED`.
pub fn code_to_json_str(code: i32) -> &'static str {
    let code = StreamErrorCode::try_from(code).unwrap_or(StreamErrorCode::Unspecified);
    code.as_str_name()
        .strip_prefix(CODE_NAME_PREFIX)
        .unwrap_or("UNSPECIFIED")
}

/// Inverse of [`code_to_json_str`]. Unknown names map to `Unspecified`.
pub fn code_from_json_str(name: &str) -> StreamErrorCode {
    StreamErrorCode::from_str_name(&format!("{CODE_NAME_PREFIX}{name}"))
        .unwrap_or(StreamErrorCode::Unspecified)
}

/// Human-readable form shared by all consumers: `ORIGIN CODE: message`.
pub fn describe(error: &StreamError) -> String {
    format!(
        "{} {}: {}",
        error.origin,
        code_to_json_str(error.code),
        error.message
    )
}

fn strip_reserved_keys(metadata: &mut HashMap<String, String>) {
    for key in RESERVED_KEYS {
        metadata.remove(key);
    }
}

fn normalize_origin(origin: String) -> String {
    if origin.is_empty() {
        tracing::warn!("stream error produced without an origin");
        return UNKNOWN_ORIGIN.to_string();
    }
    let mut origin = origin;
    origin.truncate(origin.floor_char_boundary(MAX_STREAM_ERROR_ORIGIN_BYTES));
    origin
}

/// Why a present v1 JSON value is not a valid stream error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseError {
    Oversize,
    InvalidJson,
    NotObject,
    Unversioned,
    UnsupportedVersion,
    MissingCode,
    InvalidCode,
    EmptyCode,
    MissingMessage,
    InvalidMessage,
    MissingOrigin,
    InvalidOrigin,
    EmptyOrigin,
}

fn parse_v1(encoded: &str) -> Result<StreamError, ParseError> {
    if encoded.len() > MAX_STREAM_ERROR_BYTES {
        return Err(ParseError::Oversize);
    }
    let value: Value = serde_json::from_str(encoded).map_err(|_| ParseError::InvalidJson)?;
    let fields = value.as_object().ok_or(ParseError::NotObject)?;

    match fields.get("version") {
        None => return Err(ParseError::Unversioned),
        Some(version) if version.as_u64() == Some(1) => {}
        Some(_) => return Err(ParseError::UnsupportedVersion),
    }

    let code = required_str(
        fields.get("code"),
        ParseError::MissingCode,
        ParseError::InvalidCode,
    )?;
    if code.is_empty() {
        return Err(ParseError::EmptyCode);
    }
    let message = required_str(
        fields.get("message"),
        ParseError::MissingMessage,
        ParseError::InvalidMessage,
    )?;
    let origin = required_str(
        fields.get("origin"),
        ParseError::MissingOrigin,
        ParseError::InvalidOrigin,
    )?;
    if origin.is_empty() {
        return Err(ParseError::EmptyOrigin);
    }

    Ok(StreamError {
        code: code_from_json_str(code) as i32,
        message: message.to_string(),
        origin: origin.to_string(),
    })
}

fn required_str(
    value: Option<&Value>,
    missing: ParseError,
    invalid: ParseError,
) -> Result<&str, ParseError> {
    value.ok_or(missing)?.as_str().ok_or(invalid)
}

#[derive(Serialize)]
struct StreamErrorV1<'a> {
    version: u8,
    code: &'a str,
    message: &'a str,
    origin: &'a str,
}

fn encode_v1(code: &str, message: &str, origin: &str) -> String {
    serde_json::to_string(&StreamErrorV1 {
        version: 1,
        code,
        message,
        origin,
    })
    .expect("serializing a struct of string fields cannot fail")
}

/// Encodes the v1 JSON value within [`MAX_STREAM_ERROR_BYTES`] and returns it
/// together with the (possibly truncated) message it contains, so the typed
/// field and the JSON value always carry the same message.
///
/// Escaping makes the encoded size non-linear in the message length, so the
/// longest fitting prefix is found by searching UTF-8 character boundaries.
/// The origin is already bounded, so the empty prefix always fits.
fn encode_bounded_v1(code: &str, mut message: String, origin: &str) -> (String, String) {
    let encoded = encode_v1(code, &message, origin);
    if encoded.len() <= MAX_STREAM_ERROR_BYTES {
        return (encoded, message);
    }

    let boundaries: Vec<usize> = (0..=message.len().min(MAX_STREAM_ERROR_BYTES))
        .filter(|&end| message.is_char_boundary(end))
        .collect();
    let fitting = boundaries.partition_point(|&end| {
        encode_v1(code, &message[..end], origin).len() <= MAX_STREAM_ERROR_BYTES
    });
    message.truncate(boundaries[fitting - 1]);
    (encode_v1(code, &message, origin), message)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use prost::Message;
    use serde_json::Value;

    use super::*;

    fn trailer_with_metadata(key: &str, value: &str) -> Trailer {
        Trailer {
            metadata: HashMap::from([(key.to_string(), value.to_string())]),
            ..Default::default()
        }
    }

    fn failed(trailer: &Trailer) -> StreamError {
        match inspect_end(trailer) {
            StreamErrorOutcome::Failed(error) => error,
            StreamErrorOutcome::Success => panic!("expected a failure for {trailer:?}"),
        }
    }

    fn json_of(trailer: &Trailer) -> Value {
        serde_json::from_str(trailer.metadata.get(STREAM_ERROR_METADATA_KEY).unwrap()).unwrap()
    }

    #[test]
    fn trailer_error_round_trips_through_prost() {
        let trailer = error_trailer(HashMap::new(), StreamErrorCode::Timeout, "idle", "COMMAND");
        let decoded = Trailer::decode(trailer.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, trailer);

        let decoded = Trailer::decode(Trailer::default().encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded.error, None);
    }

    #[test]
    fn unknown_enum_value_is_still_a_failure() {
        let trailer = Trailer {
            error: Some(StreamError {
                code: 99,
                message: "future".to_string(),
                origin: "FUTURE".to_string(),
            }),
            ..Default::default()
        };
        let decoded = Trailer::decode(trailer.encode_to_vec().as_slice()).unwrap();
        assert_eq!(failed(&decoded).code, 99);
        assert_eq!(result_status_for(99), ResultStatus::MaxRetry);
        assert_eq!(code_to_json_str(99), "UNSPECIFIED");
    }

    #[test]
    fn error_trailer_dual_writes_matching_json() {
        let trailer = error_trailer(
            HashMap::from([("trace_id".to_string(), "request-42".to_string())]),
            StreamErrorCode::ExecutionFailed,
            "Sandbox command exited with code 7",
            "SANDBOX",
        );

        assert_eq!(trailer.metadata.get("trace_id").unwrap(), "request-42");
        assert_eq!(
            trailer.metadata.get(STREAM_ERROR_METADATA_KEY).unwrap(),
            r#"{"version":1,"code":"EXECUTION_FAILED","message":"Sandbox command exited with code 7","origin":"SANDBOX"}"#
        );
        let error = trailer.error.clone().unwrap();
        assert_eq!(error.code, StreamErrorCode::ExecutionFailed as i32);
        assert_eq!(error.origin, "SANDBOX");
        // The JSON alone (field dropped by an older relay) decodes to the same error.
        let json_only = Trailer {
            metadata: trailer.metadata.clone(),
            error: None,
        };
        assert_eq!(failed(&json_only), error);
    }

    #[test]
    fn error_trailer_overwrites_reserved_keys() {
        let trailer = error_trailer(
            HashMap::from([
                (
                    STREAM_ERROR_METADATA_KEY.to_string(),
                    r#"{"version":1,"code":"INJECTED","message":"forged","origin":"USER"}"#
                        .to_string(),
                ),
                (
                    LEGACY_PLUGIN_STREAM_ERROR_KEY.to_string(),
                    "forged".to_string(),
                ),
            ]),
            StreamErrorCode::ExecutionFailed,
            "real failure",
            "SANDBOX",
        );
        assert_eq!(json_of(&trailer)["message"], "real failure");
        assert!(
            !trailer
                .metadata
                .contains_key(LEGACY_PLUGIN_STREAM_ERROR_KEY)
        );
    }

    #[test]
    fn message_at_the_limit_is_bounded_and_matches_the_json() {
        let overhead = encode_v1("EXECUTION_FAILED", "", "SANDBOX").len();
        let exact = "x".repeat(MAX_STREAM_ERROR_BYTES - overhead);
        let trailer = error_trailer(
            HashMap::new(),
            StreamErrorCode::ExecutionFailed,
            exact.clone(),
            "SANDBOX",
        );
        assert_eq!(trailer.error.as_ref().unwrap().message, exact);
        assert_eq!(
            trailer.metadata[STREAM_ERROR_METADATA_KEY].len(),
            MAX_STREAM_ERROR_BYTES
        );

        let over = format!("{exact}y");
        let trailer = error_trailer(
            HashMap::new(),
            StreamErrorCode::ExecutionFailed,
            over,
            "SANDBOX",
        );
        assert_eq!(trailer.error.as_ref().unwrap().message, exact);
    }

    #[test]
    fn long_messages_are_truncated_at_utf8_boundaries_consistently() {
        for message in [
            "障害💥".repeat(2_000),
            "\"\u{1}".repeat(3_000),
            "a\\\n".repeat(2_000),
        ] {
            let trailer = error_trailer(
                HashMap::new(),
                StreamErrorCode::ExecutionFailed,
                message.as_str(),
                "SANDBOX",
            );
            let encoded = &trailer.metadata[STREAM_ERROR_METADATA_KEY];
            let typed = &trailer.error.as_ref().unwrap().message;

            assert!(encoded.len() <= MAX_STREAM_ERROR_BYTES);
            assert!(typed.len() < message.len());
            assert!(message.starts_with(typed.as_str()));
            assert_eq!(json_of(&trailer)["message"], typed.as_str());
        }
    }

    #[test]
    fn json_escaping_prevents_message_content_from_injecting_fields() {
        let message = "failed\"},\"origin\":\"ATTACKER\",\"extra\":true\\\n";
        let trailer = error_trailer(
            HashMap::new(),
            StreamErrorCode::ExecutionFailed,
            message,
            "SANDBOX",
        );
        let value = json_of(&trailer);
        assert_eq!(value["origin"], "SANDBOX");
        assert_eq!(value["message"], message);
        assert_eq!(value.as_object().unwrap().len(), 4);
    }

    #[test]
    fn origin_is_never_empty_and_bounded() {
        let trailer = error_trailer(HashMap::new(), StreamErrorCode::Timeout, "m", "");
        assert_eq!(trailer.error.unwrap().origin, UNKNOWN_ORIGIN);

        let long = "原".repeat(200);
        let trailer = error_trailer(HashMap::new(), StreamErrorCode::Timeout, "m", long.clone());
        let origin = trailer.error.unwrap().origin;
        assert!(origin.len() <= MAX_STREAM_ERROR_ORIGIN_BYTES);
        assert!(long.starts_with(&origin));
        assert!(origin.len() > MAX_STREAM_ERROR_ORIGIN_BYTES - 3);
    }

    #[test]
    fn legacy_key_variant_writes_all_representations() {
        let trailer = error_trailer_with_legacy_key(
            HashMap::new(),
            StreamErrorCode::Timeout,
            "idle",
            "MyPlugin",
        );
        assert_eq!(trailer.metadata[LEGACY_PLUGIN_STREAM_ERROR_KEY], "idle");
        assert_eq!(json_of(&trailer)["code"], "TIMEOUT");
        assert_eq!(failed(&trailer).origin, "MyPlugin");
    }

    #[test]
    fn normal_trailer_keeps_metadata_and_strips_reserved_keys() {
        let trailer = normal_trailer(HashMap::from([
            ("trace_id".to_string(), "t".to_string()),
            (STREAM_ERROR_METADATA_KEY.to_string(), "x".to_string()),
            (LEGACY_PLUGIN_STREAM_ERROR_KEY.to_string(), "x".to_string()),
        ]));
        assert_eq!(
            trailer.metadata,
            HashMap::from([("trace_id".to_string(), "t".to_string())])
        );
        assert_eq!(trailer.error, None);
        assert_eq!(inspect_end(&trailer), StreamErrorOutcome::Success);
        assert!(matches!(
            normal_end_item(HashMap::new()).item,
            Some(result_output_item::Item::End(Trailer { error: None, .. }))
        ));
    }

    #[test]
    fn inspect_end_reads_compatibility_keys() {
        assert_eq!(
            inspect_end(&Trailer::default()),
            StreamErrorOutcome::Success
        );

        let error = failed(&trailer_with_metadata(
            STREAM_ERROR_METADATA_KEY,
            r#"{"version":1,"code":"TIMEOUT","message":"idle","origin":"SANDBOX","extra":1}"#,
        ));
        assert_eq!(error.code, StreamErrorCode::Timeout as i32);
        assert_eq!(error.message, "idle");
        assert_eq!(error.origin, "SANDBOX");

        let error = failed(&trailer_with_metadata(
            STREAM_ERROR_METADATA_KEY,
            r#"{"version":1,"code":"FUTURE_CODE","message":"m","origin":"X"}"#,
        ));
        assert_eq!(error.code, StreamErrorCode::Unspecified as i32);

        let error = failed(&trailer_with_metadata(
            LEGACY_PLUGIN_STREAM_ERROR_KEY,
            "plugin failed",
        ));
        assert_eq!(error.code, StreamErrorCode::ExecutionFailed as i32);
        assert_eq!(error.origin, LEGACY_PLUGIN_ORIGIN);
        assert_eq!(error.message, "plugin failed");
    }

    #[test]
    fn typed_field_wins_over_conflicting_json() {
        let trailer = Trailer {
            metadata: HashMap::from([(
                STREAM_ERROR_METADATA_KEY.to_string(),
                r#"{"version":1,"code":"TIMEOUT","message":"json","origin":"JSON"}"#.to_string(),
            )]),
            error: Some(StreamError {
                code: StreamErrorCode::Cancelled as i32,
                message: "typed".to_string(),
                origin: "TYPED".to_string(),
            }),
        };
        assert_eq!(failed(&trailer).message, "typed");
    }

    #[test]
    fn malformed_json_values_are_failures() {
        let base = r#"{"version":1,"code":"FAILED","message":"","origin":"RUNNER","padding":""}"#;
        let oversized = format!(
            r#"{{"version":1,"code":"FAILED","message":"","origin":"RUNNER","padding":"{}"}}"#,
            "x".repeat(MAX_STREAM_ERROR_BYTES - base.len() + 1)
        );
        let cases = [
            ("not-json", ParseError::InvalidJson),
            ("[]", ParseError::NotObject),
            (
                r#"{"code":"FAILED","message":"failed","origin":"RUNNER"}"#,
                ParseError::Unversioned,
            ),
            (
                r#"{"version":2,"code":"FAILED","message":"failed","origin":"RUNNER"}"#,
                ParseError::UnsupportedVersion,
            ),
            (
                r#"{"version":1,"message":"failed","origin":"RUNNER"}"#,
                ParseError::MissingCode,
            ),
            (
                r#"{"version":1,"code":"","message":"failed","origin":"RUNNER"}"#,
                ParseError::EmptyCode,
            ),
            (
                r#"{"version":1,"code":"FAILED","message":false,"origin":"RUNNER"}"#,
                ParseError::InvalidMessage,
            ),
            (
                r#"{"version":1,"code":"FAILED","message":"failed"}"#,
                ParseError::MissingOrigin,
            ),
            (oversized.as_str(), ParseError::Oversize),
        ];
        for (json, reason) in cases {
            assert_eq!(parse_v1(json), Err(reason), "{json}");
            let error = failed(&trailer_with_metadata(STREAM_ERROR_METADATA_KEY, json));
            assert_eq!(error.code, StreamErrorCode::Unspecified as i32);
            assert!(error.message.contains(&format!("{reason:?}")));
        }
    }

    #[test]
    fn ensure_end_success_exposes_the_error() {
        assert!(ensure_end_success(&Trailer::default()).is_ok());
        let trailer = error_trailer(
            HashMap::new(),
            StreamErrorCode::Cancelled,
            "stopped",
            "COMMAND",
        );
        let err = ensure_end_success(&trailer).unwrap_err();
        assert_eq!(err.to_string(), "COMMAND CANCELLED: stopped");
        let typed = err.downcast_ref::<StreamFailedError>().unwrap();
        assert_eq!(typed.0.code, StreamErrorCode::Cancelled as i32);
    }

    #[test]
    fn end_failure_returns_the_reported_error() {
        assert_eq!(end_failure(&Trailer::default()), None);
        let trailer = error_trailer(HashMap::new(), StreamErrorCode::Timeout, "idle", "COMMAND");
        assert_eq!(end_failure(&trailer), trailer.error);
    }

    #[test]
    fn success_metadata_returns_metadata_or_the_failure() {
        let metadata = HashMap::from([("trace_id".to_string(), "t".to_string())]);
        assert_eq!(
            success_metadata(normal_trailer(metadata.clone())).unwrap(),
            metadata
        );

        for trailer in [
            error_trailer(
                metadata.clone(),
                StreamErrorCode::Timeout,
                "idle",
                "COMMAND",
            ),
            trailer_with_metadata(
                STREAM_ERROR_METADATA_KEY,
                r#"{"version":1,"code":"TIMEOUT","message":"idle","origin":"COMMAND"}"#,
            ),
            trailer_with_metadata(LEGACY_PLUGIN_STREAM_ERROR_KEY, "failed"),
        ] {
            let err = success_metadata(trailer).unwrap_err();
            assert!(err.downcast_ref::<StreamFailedError>().is_some());
        }
    }

    #[test]
    fn result_status_mapping() {
        assert_eq!(
            result_status_for(StreamErrorCode::Cancelled as i32),
            ResultStatus::Cancelled
        );
        assert_eq!(
            result_status_for(StreamErrorCode::InvalidArgument as i32),
            ResultStatus::OtherError
        );
        for code in [
            StreamErrorCode::Unspecified,
            StreamErrorCode::ExecutionFailed,
            StreamErrorCode::Timeout,
            StreamErrorCode::TransportFailed,
            StreamErrorCode::ProviderStreamFailed,
        ] {
            assert_eq!(result_status_for(code as i32), ResultStatus::MaxRetry);
        }
    }

    #[test]
    fn json_code_names_round_trip() {
        for code in [
            StreamErrorCode::Unspecified,
            StreamErrorCode::ExecutionFailed,
            StreamErrorCode::Timeout,
            StreamErrorCode::Cancelled,
            StreamErrorCode::TransportFailed,
            StreamErrorCode::InvalidArgument,
            StreamErrorCode::ProviderStreamFailed,
        ] {
            assert_eq!(code_from_json_str(code_to_json_str(code as i32)), code);
        }
        assert_eq!(
            code_to_json_str(StreamErrorCode::ProviderStreamFailed as i32),
            "PROVIDER_STREAM_FAILED"
        );
        assert_eq!(code_from_json_str("NOPE"), StreamErrorCode::Unspecified);
    }
}
