use super::*;
use crate::workflow::position::WorkflowPosition;
use once_cell::sync::Lazy;
use std::collections::HashMap;

/// Macro to reduce repetitive error handling with workflow position context
///
/// This macro simplifies error handling by automatically capturing the workflow position
/// and creating appropriate error instances. It's particularly useful in async contexts
/// where task execution needs to track position information.
///
/// # Usage
///
/// Basic usage with error type and message:
/// ```ignore
/// let result = bail_with_position!(
///     task_context,
///     some_operation(),
///     bad_argument,
///     "Operation failed"
/// );
/// ```
///
/// With custom detail message:
/// ```ignore
/// let result = bail_with_position!(
///     task_context,
///     some_operation(),
///     bad_argument,
///     "Operation failed",
///     "Additional detail information"
/// );
/// ```
///
/// # Parameters
///
/// - `$task_context`: A reference to the task context containing position information
/// - `$result`: The Result to unwrap or convert to workflow error
/// - `$error_type`: The error factory method name (e.g., bad_argument, internal_error)
/// - `$message`: The error message to include in the error
/// - `$detail` (optional): Additional detail message (defaults to Debug representation of error)
#[macro_export]
macro_rules! bail_with_position {
    ($task_context:expr, $result:expr, $error_type:ident, $message:expr) => {
        match $result {
            Ok(val) => val,
            Err(e) => {
                let pos = $task_context.position.read().await;
                return Err(
                    $crate::workflow::definition::workflow::errors::ErrorFactory::new()
                        .$error_type(
                            $message.to_string(),
                            Some(pos.as_error_instance()),
                            Some(format!("{:?}", e)),
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
    pub fn is_match(&self, in_str: &str) -> bool {
        let binding = if !in_str.contains("://") {
            format!("http-error://{}", in_str.to_ascii_lowercase())
        } else {
            in_str.to_ascii_lowercase()
        };
        let in_str = binding.as_str();

        // XXX t not contains scheme, assume http-error

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
            Ok(re) => re.is_match(in_str),
            Err(_) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    BadArgument,
    Unauthorized,
    PaymentRequired,
    PermissionDenied,
    NotFound,
    MethodNotAllowed,
    RequestTimeout,
    Conflict,
    Gone,
    PreconditionFailed,
    UnsupportedMediaType,
    InternalError,
    ServiceUnavailable,
    GatewayTimeout,
    TooManyRequests,
    UnavailableForLegalReasons,
    NotImplemented,
    BadGateway,
    NotAcceptable,
    UnprocessableEntity,
    Locked,
}

static ERROR_CODE_MAP: Lazy<HashMap<ErrorCode, u16>> = Lazy::new(|| {
    let mut map = HashMap::new();
    map.insert(ErrorCode::BadArgument, 400);
    map.insert(ErrorCode::Unauthorized, 401);
    map.insert(ErrorCode::PaymentRequired, 402);
    map.insert(ErrorCode::PermissionDenied, 403);
    map.insert(ErrorCode::NotFound, 404);
    map.insert(ErrorCode::MethodNotAllowed, 405);
    map.insert(ErrorCode::RequestTimeout, 408);
    map.insert(ErrorCode::Conflict, 409);
    map.insert(ErrorCode::Gone, 410);
    map.insert(ErrorCode::PreconditionFailed, 412);
    map.insert(ErrorCode::UnsupportedMediaType, 415);
    map.insert(ErrorCode::InternalError, 500);
    map.insert(ErrorCode::ServiceUnavailable, 503);
    map.insert(ErrorCode::GatewayTimeout, 504);
    map.insert(ErrorCode::TooManyRequests, 429);
    map.insert(ErrorCode::UnavailableForLegalReasons, 451);
    map.insert(ErrorCode::NotImplemented, 501);
    map.insert(ErrorCode::BadGateway, 502);
    map.insert(ErrorCode::NotAcceptable, 406);
    map.insert(ErrorCode::UnprocessableEntity, 422);
    map.insert(ErrorCode::Locked, 423);
    map
});

#[derive(Debug, Clone, Default)]
pub struct ErrorFactory {}
impl ErrorFactory {
    pub fn new() -> Self {
        Self {}
    }
    pub fn create(
        status: ErrorCode,
        message: Option<String>,
        position: Option<String>,
        err: Option<&anyhow::Error>,
    ) -> Box<super::Error> {
        let status_code = ERROR_CODE_MAP.get(&status).copied().unwrap_or(500);
        Box::new(Error {
            title: message,
            instance: position,
            status: status_code as i64,
            detail: err.map(|e| e.to_string()),
            type_: super::UriTemplate(format!(
                "http-error://{}",
                status.to_string().to_lowercase().replace('_', "-")
            )),
        })
    }
    pub fn create_from_serde_json(
        err: &serde_json::error::Error,
        message: Option<&str>,
        code: Option<ErrorCode>,
        position: Option<&WorkflowPosition>,
    ) -> Box<super::Error> {
        let status_code = code
            .and_then(|c| ERROR_CODE_MAP.get(&c).copied())
            .unwrap_or(400);
        Box::new(Error {
            title: message.map(|s| s.to_owned()),
            instance: position.map(|p| p.as_error_instance()),
            status: status_code as i64,
            detail: Some(err.to_string()),
            type_: super::UriTemplate(format!(
                "http-error://{}",
                code.unwrap_or(ErrorCode::BadArgument)
                    .to_string()
                    .to_lowercase()
                    .replace('_', "-")
            )),
        })
    }
    pub fn create_from_liquid(
        err: &liquid::Error,
        message: Option<&str>,
        status: Option<ErrorCode>,
        position: Option<&WorkflowPosition>,
    ) -> Box<super::Error> {
        let status_code = status
            .and_then(|c| ERROR_CODE_MAP.get(&c).copied())
            .unwrap_or(400);
        Box::new(Error {
            title: message.map(|s| s.to_owned()),
            instance: position.map(|p| p.as_error_instance()),
            status: status_code as i64,
            detail: Some(err.to_string()),
            type_: super::UriTemplate(format!(
                "http-error://{}",
                status
                    .unwrap_or(ErrorCode::BadArgument)
                    .to_string()
                    .to_lowercase()
                    .replace('_', "-")
            )),
        })
    }

    // エラーコードを使用する共通ヘルパーメソッド
    fn create_error_with_code(
        &self,
        code: ErrorCode,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        let status_code = ERROR_CODE_MAP.get(&code).copied().unwrap_or(500);
        Box::new(Error {
            title: Some(message),
            instance: position,
            detail,
            status: status_code as i64,
            type_: super::UriTemplate(format!(
                "http-error://{}",
                code.to_string().to_lowercase().replace('_', "-")
            )),
        })
    }

    // エラーメソッドをErrorCodeを使って簡潔に実装
    pub fn bad_argument(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::BadArgument, message, position, detail)
    }

    pub fn unauthorized(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::Unauthorized, message, position, detail)
    }

    pub fn payment_required(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::PaymentRequired, message, position, detail)
    }

    pub fn permission_denied(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::PermissionDenied, message, position, detail)
    }

    pub fn not_found(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::NotFound, message, position, detail)
    }

    pub fn method_not_allowed(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::MethodNotAllowed, message, position, detail)
    }

    pub fn not_acceptable(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::NotAcceptable, message, position, detail)
    }

    pub fn request_timeout(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::RequestTimeout, message, position, detail)
    }

    pub fn conflict(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::Conflict, message, position, detail)
    }

    pub fn gone(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::Gone, message, position, detail)
    }

    pub fn precondition_failed(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::PreconditionFailed, message, position, detail)
    }

    pub fn unsupported_media_type(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::UnsupportedMediaType, message, position, detail)
    }

    pub fn internal_error(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::InternalError, message, position, detail)
    }

    pub fn not_implemented(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::NotImplemented, message, position, detail)
    }

    pub fn too_many_requests(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::TooManyRequests, message, position, detail)
    }

    pub fn unavailable_for_legal_reasons(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(
            ErrorCode::UnavailableForLegalReasons,
            message,
            position,
            detail,
        )
    }

    pub fn bad_gateway(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::BadGateway, message, position, detail)
    }

    pub fn service_unavailable(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::ServiceUnavailable, message, position, detail)
    }

    pub fn gateway_timeout(
        &self,
        message: String,
        position: Option<String>,
        detail: Option<String>,
    ) -> Box<super::Error> {
        self.create_error_with_code(ErrorCode::GatewayTimeout, message, position, detail)
    }

    pub fn custom(
        &self,
        title: Option<String>,
        detail: Option<String>,
        instance: Option<String>,
        status: i64,
    ) -> Box<super::Error> {
        Box::new(Error {
            title,
            detail,
            instance,
            status,
            type_: super::UriTemplate("http-error://custom".to_string()),
        })
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorCode::BadArgument => write!(f, "BadArgument"),
            ErrorCode::Unauthorized => write!(f, "Unauthorized"),
            ErrorCode::PaymentRequired => write!(f, "PaymentRequired"),
            ErrorCode::PermissionDenied => write!(f, "PermissionDenied"),
            ErrorCode::NotFound => write!(f, "NotFound"),
            ErrorCode::MethodNotAllowed => write!(f, "MethodNotAllowed"),
            ErrorCode::RequestTimeout => write!(f, "RequestTimeout"),
            ErrorCode::Conflict => write!(f, "Conflict"),
            ErrorCode::Gone => write!(f, "Gone"),
            ErrorCode::PreconditionFailed => write!(f, "PreconditionFailed"),
            ErrorCode::UnsupportedMediaType => write!(f, "UnsupportedMediaType"),
            ErrorCode::InternalError => write!(f, "InternalError"),
            ErrorCode::ServiceUnavailable => write!(f, "ServiceUnavailable"),
            ErrorCode::GatewayTimeout => write!(f, "GatewayTimeout"),
            ErrorCode::TooManyRequests => write!(f, "TooManyRequests"),
            ErrorCode::UnavailableForLegalReasons => write!(f, "UnavailableForLegalReasons"),
            ErrorCode::NotImplemented => write!(f, "NotImplemented"),
            ErrorCode::BadGateway => write!(f, "BadGateway"),
            ErrorCode::NotAcceptable => write!(f, "NotAcceptable"),
            ErrorCode::UnprocessableEntity => write!(f, "UnprocessableEntity"),
            ErrorCode::Locked => write!(f, "Locked"),
        }
    }
}
