use super::*;
use crate::workflow::execute::context::WorkflowPosition;
use once_cell::sync::Lazy;
use std::collections::HashMap;

impl super::Error {
    pub fn position(&mut self, position: &WorkflowPosition) -> &mut Self {
        self.instance = Some(position.as_error_instance());
        self
    }
    pub fn message(&mut self, message: &str) -> &mut Self {
        self.title = Some(ErrorTitle {
            subtype_0: Some(super::RuntimeExpression(message.to_string())),
            subtype_1: self.title.as_ref().and_then(|t| t.subtype_1.clone()),
        });
        self
    }
}
impl std::error::Error for super::Error {}

impl std::fmt::Display for super::Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Error: {} {} {}",
            self.title
                .as_ref()
                .and_then(|t| t
                    .subtype_0
                    .as_ref()
                    .map(|s| s.0.as_str())
                    .or(t.subtype_1.as_deref()))
                .unwrap_or("Unknown error"),
            self.detail
                .as_ref()
                .and_then(|d| d
                    .subtype_0
                    .as_ref()
                    .map(|s| s.0.as_str())
                    .or(d.subtype_1.as_deref()))
                .unwrap_or("No details"),
            self.instance
                .as_ref()
                .unwrap_or(&ErrorInstance::LiteralErrorInstance(
                    "No instance".to_string()
                )),
        )
    }
}

impl From<String> for ErrorTitle {
    fn from(s: String) -> Self {
        ErrorTitle {
            subtype_0: Some(super::RuntimeExpression(s)),
            subtype_1: None,
        }
    }
}
impl From<String> for ErrorDetails {
    fn from(s: String) -> Self {
        ErrorDetails {
            subtype_0: Some(super::RuntimeExpression(s)),
            subtype_1: None,
        }
    }
}
impl From<anyhow::Error> for ErrorDetails {
    fn from(s: anyhow::Error) -> Self {
        ErrorDetails {
            subtype_0: Some(super::RuntimeExpression(s.to_string())),
            subtype_1: None,
        }
    }
}
impl From<&anyhow::Error> for ErrorDetails {
    fn from(s: &anyhow::Error) -> Self {
        ErrorDetails {
            subtype_0: Some(super::RuntimeExpression(s.to_string())),
            subtype_1: None,
        }
    }
}
impl ErrorType {
    pub fn is_match(&self, in_str: &str) -> bool {
        match self {
            ErrorType::UriTemplate(templ) => {
                tracing::debug!("is_match: {:?} <=> {}", templ, in_str);
                // XXX implicitly convert string to UriTemplate (http-error://)
                templ.is_match(in_str)
                    || (!in_str.contains("://") // XXX t not contains scheme, assume http-error
                        && templ.is_match(&format!("http-error://{}", in_str.to_ascii_lowercase())))
            }
            ErrorType::RuntimeExpression(exp) => exp.0.as_str() == in_str,
        }
    }
}
impl UriTemplate {
    pub fn is_match(&self, in_str: &str) -> bool {
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
                pattern.push_str(&regex::escape(&format!("{{{}", part)));
            }
        }

        match regex::Regex::new(&format!("^{}$", pattern)) {
            Ok(re) => re.is_match(in_str),
            Err(_) => false,
        }
    }
}

// impl From<String> for ErrorType {
//     fn from(s: String) -> Self {
//         ErrorType::RuntimeExpression(super::RuntimeExpression(s))
//     }
// }

impl ToString for ErrorTitle {
    fn to_string(&self) -> String {
        self.subtype_0
            .as_ref()
            .map(|s| s.0.as_str())
            .or(self.subtype_1.as_deref())
            .unwrap_or("")
            .to_string()
    }
}
impl ToString for ErrorDetails {
    fn to_string(&self) -> String {
        self.subtype_0
            .as_ref()
            .map(|s| s.0.as_str())
            .or(self.subtype_1.as_deref())
            .unwrap_or("")
            .to_string()
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

pub struct ErrorFactory {}
impl ErrorFactory {
    pub fn new() -> Self {
        Self {}
    }
    pub fn create(
        status: ErrorCode,
        message: Option<String>,
        position: Option<&WorkflowPosition>,
        err: Option<&anyhow::Error>,
    ) -> super::Error {
        let status_code = ERROR_CODE_MAP.get(&status).copied().unwrap_or(500);
        Error {
            title: message.map(|d| ErrorTitle {
                subtype_0: Some(super::RuntimeExpression(d)),
                subtype_1: None,
            }),
            instance: position.map(|p| p.as_error_instance()),
            status: status_code as i64,
            detail: err.map(|e| e.into()),
            type_: ErrorType::UriTemplate(super::UriTemplate(format!(
                "http-error://{}",
                status.to_string().to_lowercase().replace('_', "-")
            ))),
        }
    }
    pub fn create_from_serde_json(
        err: &serde_json::error::Error,
        message: Option<&str>,
        code: Option<ErrorCode>,
        position: Option<&WorkflowPosition>,
    ) -> super::Error {
        let status_code = code
            .and_then(|c| ERROR_CODE_MAP.get(&c).copied())
            .unwrap_or(400);
        Error {
            title: message.map(|d| ErrorTitle {
                subtype_0: Some(super::RuntimeExpression(d.to_string())),
                subtype_1: None,
            }),
            instance: position.map(|p| p.as_error_instance()),
            status: status_code as i64,
            detail: Some(ErrorDetails {
                subtype_0: Some(super::RuntimeExpression(err.to_string())),
                subtype_1: None,
            }),
            type_: ErrorType::UriTemplate(super::UriTemplate(format!(
                "http-error://{}",
                code.unwrap_or(ErrorCode::BadArgument)
                    .to_string()
                    .to_lowercase()
                    .replace('_', "-")
            ))),
        }
    }
    pub fn create_from_liquid(
        err: &liquid::Error,
        message: Option<&str>,
        status: Option<ErrorCode>,
        position: Option<&WorkflowPosition>,
    ) -> super::Error {
        let status_code = status
            .and_then(|c| ERROR_CODE_MAP.get(&c).copied())
            .unwrap_or(400);
        Error {
            title: message.map(|d| ErrorTitle {
                subtype_0: Some(super::RuntimeExpression(d.to_string())),
                subtype_1: None,
            }),
            instance: position.map(|p| p.as_error_instance()),
            status: status_code as i64,
            detail: Some(ErrorDetails {
                subtype_0: Some(super::RuntimeExpression(err.to_string())),
                subtype_1: None,
            }),
            type_: ErrorType::UriTemplate(super::UriTemplate(format!(
                "http-error://{}",
                status
                    .unwrap_or(ErrorCode::BadArgument)
                    .to_string()
                    .to_lowercase()
                    .replace('_', "-")
            ))),
        }
    }

    // エラーコードを使用する共通ヘルパーメソッド
    fn create_error_with_code(
        &self,
        code: ErrorCode,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        let status_code = ERROR_CODE_MAP.get(&code).copied().unwrap_or(500);
        Error {
            title: Some(ErrorTitle {
                subtype_0: Some(RuntimeExpression(message)),
                subtype_1: None,
            }),
            instance: position.map(|p| p.as_error_instance()),
            detail,
            status: status_code as i64,
            type_: ErrorType::UriTemplate(super::UriTemplate(format!(
                "http-error://{}",
                code.to_string().to_lowercase().replace('_', "-")
            ))),
        }
    }

    // エラーメソッドをErrorCodeを使って簡潔に実装
    pub fn bad_argument(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::BadArgument, message, position, detail)
    }

    pub fn unauthorized(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::Unauthorized, message, position, detail)
    }

    pub fn payment_required(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::PaymentRequired, message, position, detail)
    }

    pub fn permission_denied(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::PermissionDenied, message, position, detail)
    }

    pub fn not_found(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::NotFound, message, position, detail)
    }

    pub fn method_not_allowed(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::MethodNotAllowed, message, position, detail)
    }

    pub fn not_acceptable(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::NotAcceptable, message, position, detail)
    }

    pub fn request_timeout(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::RequestTimeout, message, position, detail)
    }

    pub fn conflict(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::Conflict, message, position, detail)
    }

    pub fn gone(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::Gone, message, position, detail)
    }

    pub fn precondition_failed(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::PreconditionFailed, message, position, detail)
    }

    pub fn unsupported_media_type(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::UnsupportedMediaType, message, position, detail)
    }

    pub fn internal_error(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::InternalError, message, position, detail)
    }

    pub fn not_implemented(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::NotImplemented, message, position, detail)
    }

    pub fn too_many_requests(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::TooManyRequests, message, position, detail)
    }

    pub fn unavailable_for_legal_reasons(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
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
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::BadGateway, message, position, detail)
    }

    pub fn service_unavailable(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::ServiceUnavailable, message, position, detail)
    }

    pub fn gateway_timeout(
        &self,
        message: String,
        position: Option<&WorkflowPosition>,
        detail: Option<ErrorDetails>,
    ) -> Error {
        self.create_error_with_code(ErrorCode::GatewayTimeout, message, position, detail)
    }

    pub fn custom(
        &self,
        title: Option<ErrorTitle>,
        detail: Option<ErrorDetails>,
        instance: Option<ErrorInstance>,
        status: i64,
    ) -> Error {
        Error {
            title,
            detail,
            instance,
            status,
            type_: ErrorType::UriTemplate(super::UriTemplate("http-error://custom".to_string())),
        }
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
