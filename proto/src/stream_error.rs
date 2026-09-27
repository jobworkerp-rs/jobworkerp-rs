use std::{collections::HashMap, error::Error, fmt};

use serde::Serialize;
use serde_json::Value;

use crate::jobworkerp::data::Trailer;

/// Reserved Trailer metadata key for a common streaming failure.
pub const STREAM_ERROR_METADATA_KEY: &str = "stream_error";

/// Maximum serialized size of a stream error value, measured in UTF-8 bytes.
pub const MAX_STREAM_ERROR_BYTES: usize = 4096;

/// The normalized fields of a supported v1 stream error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamError {
    pub code: String,
    pub message: String,
    pub origin: String,
}

/// The result of inspecting a stream's End Trailer for a common error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamErrorOutcome {
    Missing,
    Error(StreamError),
    Malformed(StreamErrorParseError),
}

/// Why a present stream error value is not a valid v1 error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamErrorParseError {
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

/// Why a trusted stream error could not be added to a Trailer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamErrorBuildError {
    EmptyCode,
    EmptyOrigin,
    FieldsTooLarge,
    ReservedMetadataKey,
    Serialization,
}

impl fmt::Display for StreamErrorBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCode => formatter.write_str("stream error code must not be empty"),
            Self::EmptyOrigin => formatter.write_str("stream error origin must not be empty"),
            Self::FieldsTooLarge => {
                formatter.write_str("stream error code or origin exceeds the size limit")
            }
            Self::ReservedMetadataKey => formatter
                .write_str("untrusted metadata already contains the reserved stream_error key"),
            Self::Serialization => formatter.write_str("failed to serialize stream error JSON"),
        }
    }
}

impl Error for StreamErrorBuildError {}

/// Adds a bounded v1 stream error to an End Trailer.
///
/// Existing metadata is preserved, but a preexisting reserved key is rejected
/// so caller-controlled metadata cannot impersonate a trusted Runner error.
/// JSON serialization escapes message content, and only the message is
/// truncated when needed to keep the complete JSON value within the byte limit.
pub fn build_stream_error_trailer(
    mut metadata: HashMap<String, String>,
    code: impl Into<String>,
    message: impl Into<String>,
    origin: impl Into<String>,
) -> Result<Trailer, StreamErrorBuildError> {
    if metadata.contains_key(STREAM_ERROR_METADATA_KEY) {
        return Err(StreamErrorBuildError::ReservedMetadataKey);
    }

    let error = StreamError {
        code: code.into(),
        message: message.into(),
        origin: origin.into(),
    };
    if error.code.is_empty() {
        return Err(StreamErrorBuildError::EmptyCode);
    }
    if error.origin.is_empty() {
        return Err(StreamErrorBuildError::EmptyOrigin);
    }
    if error.code.len() > MAX_STREAM_ERROR_BYTES || error.origin.len() > MAX_STREAM_ERROR_BYTES {
        return Err(StreamErrorBuildError::FieldsTooLarge);
    }

    let encoded = encode_bounded_v1(&error)?;
    metadata.insert(
        STREAM_ERROR_METADATA_KEY.to_string(),
        String::from_utf8(encoded).map_err(|_| StreamErrorBuildError::Serialization)?,
    );
    Ok(Trailer { metadata })
}

/// Validates the reserved key in an End Trailer without treating bad data as success.
pub fn parse_stream_error(trailer: &Trailer) -> StreamErrorOutcome {
    let Some(encoded) = trailer.metadata.get(STREAM_ERROR_METADATA_KEY) else {
        return StreamErrorOutcome::Missing;
    };
    if encoded.len() > MAX_STREAM_ERROR_BYTES {
        return StreamErrorOutcome::Malformed(StreamErrorParseError::Oversize);
    }

    let value: Value = match serde_json::from_str(encoded) {
        Ok(value) => value,
        Err(_) => return StreamErrorOutcome::Malformed(StreamErrorParseError::InvalidJson),
    };
    let Some(fields) = value.as_object() else {
        return StreamErrorOutcome::Malformed(StreamErrorParseError::NotObject);
    };

    match fields.get("version") {
        None => return StreamErrorOutcome::Malformed(StreamErrorParseError::Unversioned),
        Some(version) if version.as_u64() == Some(1) => {}
        Some(_) => {
            return StreamErrorOutcome::Malformed(StreamErrorParseError::UnsupportedVersion);
        }
    }

    let code = match fields.get("code") {
        None => return StreamErrorOutcome::Malformed(StreamErrorParseError::MissingCode),
        Some(value) => match value.as_str() {
            None => return StreamErrorOutcome::Malformed(StreamErrorParseError::InvalidCode),
            Some("") => return StreamErrorOutcome::Malformed(StreamErrorParseError::EmptyCode),
            Some(code) => code,
        },
    };
    let message = match fields.get("message") {
        None => return StreamErrorOutcome::Malformed(StreamErrorParseError::MissingMessage),
        Some(value) => match value.as_str() {
            None => return StreamErrorOutcome::Malformed(StreamErrorParseError::InvalidMessage),
            Some(message) => message,
        },
    };
    let origin = match fields.get("origin") {
        None => return StreamErrorOutcome::Malformed(StreamErrorParseError::MissingOrigin),
        Some(value) => match value.as_str() {
            None => return StreamErrorOutcome::Malformed(StreamErrorParseError::InvalidOrigin),
            Some("") => return StreamErrorOutcome::Malformed(StreamErrorParseError::EmptyOrigin),
            Some(origin) => origin,
        },
    };

    StreamErrorOutcome::Error(StreamError {
        code: code.to_string(),
        message: message.to_string(),
        origin: origin.to_string(),
    })
}

#[derive(Serialize)]
struct StreamErrorV1<'a> {
    version: u8,
    code: &'a str,
    message: &'a str,
    origin: &'a str,
}

fn encode_v1(error: &StreamError, message: &str) -> Result<Vec<u8>, StreamErrorBuildError> {
    serde_json::to_vec(&StreamErrorV1 {
        version: 1,
        code: &error.code,
        message,
        origin: &error.origin,
    })
    .map_err(|_| StreamErrorBuildError::Serialization)
}

fn encode_bounded_v1(error: &StreamError) -> Result<Vec<u8>, StreamErrorBuildError> {
    let empty_message = encode_v1(error, "")?;
    if empty_message.len() > MAX_STREAM_ERROR_BYTES {
        return Err(StreamErrorBuildError::FieldsTooLarge);
    }

    let max_prefix_bytes = error.message.len().min(MAX_STREAM_ERROR_BYTES);
    let mut boundaries = Vec::new();
    boundaries.push(0);
    for (index, _) in error.message.char_indices() {
        if index > max_prefix_bytes {
            break;
        }
        if index > 0 {
            boundaries.push(index);
        }
    }
    if error.message.is_char_boundary(max_prefix_bytes)
        && boundaries.last().copied() != Some(max_prefix_bytes)
    {
        boundaries.push(max_prefix_bytes);
    }

    let mut low = 0;
    let mut high = boundaries.len();
    while low < high {
        let middle = low + (high - low) / 2;
        let prefix = &error.message[..boundaries[middle]];
        if encode_v1(error, prefix)?.len() <= MAX_STREAM_ERROR_BYTES {
            low = middle + 1;
        } else {
            high = middle;
        }
    }

    let longest_valid_prefix = boundaries[low - 1];
    encode_v1(error, &error.message[..longest_valid_prefix])
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::Value;

    use crate::jobworkerp::data::Trailer;

    use super::{
        MAX_STREAM_ERROR_BYTES, StreamError, StreamErrorBuildError, StreamErrorOutcome,
        StreamErrorParseError, build_stream_error_trailer, parse_stream_error,
    };

    fn trailer_with_json(value: &str) -> Trailer {
        Trailer {
            metadata: HashMap::from([("stream_error".to_string(), value.to_string())]),
        }
    }

    #[test]
    fn builds_a_v1_error_trailer_and_round_trips() {
        let trailer = build_stream_error_trailer(
            HashMap::from([("trace_id".to_string(), "request-42".to_string())]),
            "EXECUTION_FAILED",
            "Sandbox command exited with code 7",
            "SANDBOX",
        )
        .unwrap();

        assert_eq!(trailer.metadata.get("trace_id").unwrap(), "request-42");
        assert_eq!(
            trailer.metadata.get("stream_error").unwrap(),
            r#"{"version":1,"code":"EXECUTION_FAILED","message":"Sandbox command exited with code 7","origin":"SANDBOX"}"#
        );
        assert_eq!(
            parse_stream_error(&trailer),
            StreamErrorOutcome::Error(StreamError {
                code: "EXECUTION_FAILED".to_string(),
                message: "Sandbox command exited with code 7".to_string(),
                origin: "SANDBOX".to_string(),
            })
        );
    }

    #[test]
    fn absent_reserved_key_is_distinct_from_an_error() {
        assert_eq!(
            parse_stream_error(&Trailer {
                metadata: HashMap::new(),
            }),
            StreamErrorOutcome::Missing
        );
    }

    #[test]
    fn rejects_reserved_metadata_instead_of_trusting_an_injected_value() {
        let result = build_stream_error_trailer(
            HashMap::from([(
                "stream_error".to_string(),
                r#"{"version":1,"code":"INJECTED","message":"forged","origin":"USER"}"#.to_string(),
            )]),
            "EXECUTION_FAILED",
            "real failure",
            "SANDBOX",
        );

        assert_eq!(
            result.unwrap_err(),
            StreamErrorBuildError::ReservedMetadataKey
        );
    }

    #[test]
    fn accepts_unknown_codes_and_ignores_additional_v1_fields() {
        let trailer = trailer_with_json(
            r#"{"version":1,"code":"FUTURE_CODE","message":"failed","origin":"OTHER","detail":{"retryable":false}}"#,
        );

        assert_eq!(
            parse_stream_error(&trailer),
            StreamErrorOutcome::Error(StreamError {
                code: "FUTURE_CODE".to_string(),
                message: "failed".to_string(),
                origin: "OTHER".to_string(),
            })
        );
    }

    #[test]
    fn malformed_and_unversioned_values_are_not_treated_as_success() {
        let cases = [
            ("not-json", StreamErrorParseError::InvalidJson),
            ("[]", StreamErrorParseError::NotObject),
            (
                r#"{"code":"FAILED","message":"failed","origin":"RUNNER"}"#,
                StreamErrorParseError::Unversioned,
            ),
            (
                r#"{"version":2,"code":"FAILED","message":"failed","origin":"RUNNER"}"#,
                StreamErrorParseError::UnsupportedVersion,
            ),
            (
                r#"{"version":1,"message":"failed","origin":"RUNNER"}"#,
                StreamErrorParseError::MissingCode,
            ),
            (
                r#"{"version":1,"code":"","message":"failed","origin":"RUNNER"}"#,
                StreamErrorParseError::EmptyCode,
            ),
            (
                r#"{"version":1,"code":"FAILED","message":false,"origin":"RUNNER"}"#,
                StreamErrorParseError::InvalidMessage,
            ),
            (
                r#"{"version":1,"code":"FAILED","message":"failed"}"#,
                StreamErrorParseError::MissingOrigin,
            ),
        ];

        for (json, expected) in cases {
            assert_eq!(
                parse_stream_error(&trailer_with_json(json)),
                StreamErrorOutcome::Malformed(expected),
                "unexpected parse result for {json}"
            );
        }
    }

    #[test]
    fn enforces_the_size_limit_in_bytes_including_the_exact_boundary() {
        let base = r#"{"version":1,"code":"FAILED","message":"","origin":"RUNNER","padding":""}"#;
        let exact_boundary = format!(
            r#"{{"version":1,"code":"FAILED","message":"","origin":"RUNNER","padding":"{}"}}"#,
            "x".repeat(MAX_STREAM_ERROR_BYTES - base.len())
        );
        assert_eq!(exact_boundary.len(), MAX_STREAM_ERROR_BYTES);
        assert!(matches!(
            parse_stream_error(&trailer_with_json(&exact_boundary)),
            StreamErrorOutcome::Error(_)
        ));

        let oversized = format!("{exact_boundary} ");
        assert_eq!(
            parse_stream_error(&trailer_with_json(&oversized)),
            StreamErrorOutcome::Malformed(StreamErrorParseError::Oversize)
        );
    }

    #[test]
    fn bounds_long_messages_at_utf8_character_boundaries() {
        let message = "障害💥".repeat(2_000);
        let trailer = build_stream_error_trailer(
            HashMap::new(),
            "EXECUTION_FAILED",
            message.as_str(),
            "SANDBOX",
        )
        .unwrap();
        let encoded = trailer.metadata.get("stream_error").unwrap();

        assert!(encoded.len() <= MAX_STREAM_ERROR_BYTES);
        let parsed = match parse_stream_error(&trailer) {
            StreamErrorOutcome::Error(error) => error,
            outcome => panic!("expected a valid bounded error, got {outcome:?}"),
        };
        assert!(message.starts_with(&parsed.message));
        assert!(parsed.message.len() < message.len());
        assert_eq!(
            message.get(..parsed.message.len()),
            Some(parsed.message.as_str())
        );
    }

    #[test]
    fn json_escaping_prevents_message_content_from_injecting_fields() {
        let message = "failed\"},\"origin\":\"ATTACKER\",\"extra\":true\\\n";
        let trailer =
            build_stream_error_trailer(HashMap::new(), "EXECUTION_FAILED", message, "SANDBOX")
                .unwrap();
        let json = trailer.metadata.get("stream_error").unwrap();
        let value: Value = serde_json::from_str(json).unwrap();

        assert_eq!(value["origin"], "SANDBOX");
        assert_eq!(value["message"], message);
        assert_eq!(value.as_object().unwrap().len(), 4);
        assert_eq!(
            parse_stream_error(&trailer),
            StreamErrorOutcome::Error(StreamError {
                code: "EXECUTION_FAILED".to_string(),
                message: message.to_string(),
                origin: "SANDBOX".to_string(),
            })
        );
    }

    #[test]
    fn builder_rejects_empty_required_identifiers_and_unbounded_identifiers() {
        assert_eq!(
            build_stream_error_trailer(HashMap::new(), "", "message", "SANDBOX").unwrap_err(),
            StreamErrorBuildError::EmptyCode
        );
        assert_eq!(
            build_stream_error_trailer(HashMap::new(), "FAILED", "message", "").unwrap_err(),
            StreamErrorBuildError::EmptyOrigin
        );
        assert_eq!(
            build_stream_error_trailer(
                HashMap::new(),
                "x".repeat(MAX_STREAM_ERROR_BYTES + 1),
                "message",
                "SANDBOX",
            )
            .unwrap_err(),
            StreamErrorBuildError::FieldsTooLarge
        );
    }
}
