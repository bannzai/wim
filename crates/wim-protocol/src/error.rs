//! What a failed request answers with.

use serde::{Deserialize, Serialize};

/// The `error` payload of a response: `{"code":"not_found","message":"..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseError {
    /// Machine readable reason, matched on by clients.
    pub code: ErrorCode,
    /// Human readable detail, shown to the user as is.
    pub message: String,
}

impl ResponseError {
    /// An error with `code` and `message`.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Reason a request failed.
///
/// Unknown codes are kept as [`ErrorCode::Other`] rather than rejected, so that a client keeps
/// working against a daemon that has learned a code it does not know yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ErrorCode {
    /// No `auth` message has been sent yet, or the token did not match.
    Unauthorized,
    /// The message did not parse, or its `params` did not fit the method.
    InvalidRequest,
    /// The protocol version the message carries is not one this side speaks.
    UnsupportedVersion,
    /// The path does not exist.
    NotFound,
    /// The path exists but the daemon may not touch it.
    PermissionDenied,
    /// The file system operation failed.
    Io,
    /// The daemon hit a bug.
    Internal,
    /// A code this build does not know, kept as it arrived.
    Other(String),
}

impl ErrorCode {
    /// The name this code has on the wire.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::InvalidRequest => "invalid_request",
            Self::UnsupportedVersion => "unsupported_version",
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::Io => "io",
            Self::Internal => "internal",
            Self::Other(code) => code,
        }
    }
}

impl From<String> for ErrorCode {
    fn from(code: String) -> Self {
        match code.as_str() {
            "unauthorized" => Self::Unauthorized,
            "invalid_request" => Self::InvalidRequest,
            "unsupported_version" => Self::UnsupportedVersion,
            "not_found" => Self::NotFound,
            "permission_denied" => Self::PermissionDenied,
            "io" => Self::Io,
            "internal" => Self::Internal,
            _ => Self::Other(code),
        }
    }
}

impl From<ErrorCode> for String {
    fn from(code: ErrorCode) -> Self {
        match code {
            ErrorCode::Other(code) => code,
            code => code.as_str().to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_serializes_to_a_code_and_a_message() {
        let error = ResponseError::new(ErrorCode::NotFound, "no such file: /tmp/nope");
        let json = serde_json::to_string(&error).expect("error should serialize");
        assert_eq!(
            json,
            r#"{"code":"not_found","message":"no such file: /tmp/nope"}"#
        );
        assert_eq!(
            serde_json::from_str::<ResponseError>(&json).expect("error should parse"),
            error
        );
    }

    #[test]
    fn every_known_code_roundtrips_through_its_wire_name() {
        for code in [
            ErrorCode::Unauthorized,
            ErrorCode::InvalidRequest,
            ErrorCode::UnsupportedVersion,
            ErrorCode::NotFound,
            ErrorCode::PermissionDenied,
            ErrorCode::Io,
            ErrorCode::Internal,
        ] {
            let json = serde_json::to_string(&code).expect("code should serialize");
            assert_eq!(json, format!("\"{}\"", code.as_str()));
            assert_eq!(
                serde_json::from_str::<ErrorCode>(&json).expect("code should parse"),
                code
            );
        }
    }

    #[test]
    fn a_code_this_build_does_not_know_is_kept_as_it_arrived() {
        let code: ErrorCode =
            serde_json::from_str(r#""quota_exceeded""#).expect("code should parse");
        assert_eq!(code, ErrorCode::Other("quota_exceeded".to_owned()));
        assert_eq!(
            serde_json::to_string(&code).expect("code should serialize"),
            r#""quota_exceeded""#
        );
    }
}
