//! The three envelopes that cross the wire, each carrying the protocol version as `v`.

use serde::{Deserialize, Serialize};

use crate::error::ResponseError;
use crate::fs::{
    AuthParams, FsChangedParams, FsListParams, FsReadParams, FsUnwatchParams, FsWatchParams,
    FsWriteParams,
};

/// Version of the protocol these types speak, sent as `v` on every message.
pub const PROTOCOL_VERSION: u32 = 1;

/// Whether a message tagged with `version` can be handled by this build.
///
/// A message of another version still parses, so that the side reading it can answer with
/// [`crate::ErrorCode::UnsupportedVersion`] instead of dropping the connection blind.
pub fn is_supported_version(version: u32) -> bool {
    version == PROTOCOL_VERSION
}

/// A client asking the daemon for something: `{"v":1,"id":1,"method":"fs.read","params":{...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Protocol version.
    #[serde(rename = "v")]
    pub version: u32,
    /// Names the response that answers this request. Chosen by the client.
    pub id: u64,
    /// The method and its params.
    #[serde(flatten)]
    pub method: Method,
}

impl Request {
    /// A request for the current protocol version.
    pub fn new(id: u64, method: Method) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            method,
        }
    }
}

/// What a request asks for, tagged by `method` with its params under `params`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Method {
    /// The first message of a connection. Anything else before it is refused.
    #[serde(rename = "auth")]
    Auth(AuthParams),
    /// Lists a directory. Answers with [`crate::FsListResult`].
    #[serde(rename = "fs.list")]
    FsList(FsListParams),
    /// Reads a file whole. Answers with [`crate::FsReadResult`].
    #[serde(rename = "fs.read")]
    FsRead(FsReadParams),
    /// Writes a file whole. Answers with [`crate::Ack`].
    #[serde(rename = "fs.write")]
    FsWrite(FsWriteParams),
    /// Starts reporting changes under a path. Answers with [`crate::FsWatchResult`], then
    /// [`Event::FsChanged`] pushes until the watch is dropped.
    #[serde(rename = "fs.watch")]
    FsWatch(FsWatchParams),
    /// Drops a watch. Answers with [`crate::Ack`].
    #[serde(rename = "fs.unwatch")]
    FsUnwatch(FsUnwatchParams),
}

/// The daemon answering one request: `{"v":1,"id":1,"result":...}` or `{"v":1,"id":1,"error":{...}}`.
///
/// Which type the result holds depends on the method of the request `id` names, and the daemon is
/// free to grow the results it sends, so the result stays a [`serde_json::Value`] here. The client
/// knows the method it sent and reads it back with `serde_json::from_value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Protocol version.
    #[serde(rename = "v")]
    pub version: u32,
    /// The `id` of the request being answered.
    pub id: u64,
    /// The outcome.
    #[serde(flatten)]
    pub payload: ResponsePayload,
}

impl Response {
    /// A response carrying the result of the method the request `id` asked for.
    pub fn ok<T: Serialize>(id: u64, result: &T) -> Result<Self, serde_json::Error> {
        Ok(Self {
            version: PROTOCOL_VERSION,
            id,
            payload: ResponsePayload::Result(serde_json::to_value(result)?),
        })
    }

    /// A response saying the request failed.
    pub fn err(id: u64, error: ResponseError) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            payload: ResponsePayload::Error(error),
        }
    }

    /// The result, `None` when the request failed.
    pub fn result(&self) -> Option<&serde_json::Value> {
        match &self.payload {
            ResponsePayload::Result(result) => Some(result),
            ResponsePayload::Error(_) => None,
        }
    }

    /// The error, `None` when the request succeeded.
    pub fn error(&self) -> Option<&ResponseError> {
        match &self.payload {
            ResponsePayload::Result(_) => None,
            ResponsePayload::Error(error) => Some(error),
        }
    }
}

/// How a response turned out. Exactly one of `result` and `error` is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponsePayload {
    /// The method ran, and this is what it returned.
    Result(serde_json::Value),
    /// The method did not run.
    Error(ResponseError),
}

/// The daemon reporting something no request asked for: `{"v":1,"event":"...","params":{...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerPush {
    /// Protocol version.
    #[serde(rename = "v")]
    pub version: u32,
    /// The event and its params.
    #[serde(flatten)]
    pub event: Event,
}

impl ServerPush {
    /// A push for the current protocol version.
    pub fn new(event: Event) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            event,
        }
    }
}

/// What a push reports, tagged by `event` with its params under `params`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", content = "params")]
pub enum Event {
    /// A path under a watch changed.
    #[serde(rename = "fs.changed")]
    FsChanged(FsChangedParams),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::fs::{
        Ack, AuthResult, DirEntry, EntryKind, FsChangeKind, FsListResult, FsReadResult,
        FsWatchResult,
    };

    #[test]
    fn an_auth_request_serializes_to_the_documented_json() {
        let request = Request::new(
            0,
            Method::Auth(AuthParams {
                token: "s3cret".to_owned(),
            }),
        );
        let json = serde_json::to_string(&request).expect("request should serialize");
        assert_eq!(
            json,
            r#"{"v":1,"id":0,"method":"auth","params":{"token":"s3cret"}}"#
        );
        assert_eq!(
            serde_json::from_str::<Request>(&json).expect("request should parse"),
            request
        );
    }

    #[test]
    fn a_write_request_serializes_to_the_documented_json() {
        let request = Request::new(
            12,
            Method::FsWrite(FsWriteParams {
                path: "/tmp/notes.md".to_owned(),
                content: "hello\n".to_owned(),
            }),
        );
        assert_eq!(
            serde_json::to_string(&request).expect("request should serialize"),
            r#"{"v":1,"id":12,"method":"fs.write","params":{"path":"/tmp/notes.md","content":"hello\n"}}"#
        );
    }

    #[test]
    fn every_method_roundtrips_through_json() {
        let methods = [
            Method::Auth(AuthParams {
                token: "s3cret".to_owned(),
            }),
            Method::FsList(FsListParams {
                path: "/tmp".to_owned(),
            }),
            Method::FsRead(FsReadParams {
                path: "/tmp/notes.md".to_owned(),
            }),
            Method::FsWrite(FsWriteParams {
                path: "/tmp/notes.md".to_owned(),
                content: "hello\n".to_owned(),
            }),
            Method::FsWatch(FsWatchParams {
                path: "/tmp".to_owned(),
                recursive: true,
            }),
            Method::FsUnwatch(FsUnwatchParams { watch_id: 7 }),
        ];
        for (id, method) in methods.into_iter().enumerate() {
            let request = Request::new(id as u64, method);
            let json = serde_json::to_string(&request).expect("request should serialize");
            assert_eq!(
                serde_json::from_str::<Request>(&json).expect("request should parse"),
                request
            );
        }
    }

    #[test]
    fn the_method_names_are_the_ones_the_daemon_serves() {
        let names = [
            (
                Method::Auth(AuthParams {
                    token: String::new(),
                }),
                "auth",
            ),
            (
                Method::FsList(FsListParams {
                    path: String::new(),
                }),
                "fs.list",
            ),
            (
                Method::FsRead(FsReadParams {
                    path: String::new(),
                }),
                "fs.read",
            ),
            (
                Method::FsWrite(FsWriteParams {
                    path: String::new(),
                    content: String::new(),
                }),
                "fs.write",
            ),
            (
                Method::FsWatch(FsWatchParams {
                    path: String::new(),
                    recursive: false,
                }),
                "fs.watch",
            ),
            (
                Method::FsUnwatch(FsUnwatchParams { watch_id: 0 }),
                "fs.unwatch",
            ),
        ];
        for (method, name) in names {
            let json = serde_json::to_value(Request::new(0, method)).expect("should serialize");
            assert_eq!(json["method"], serde_json::json!(name));
        }
    }

    #[test]
    fn a_result_response_serializes_to_the_documented_json() {
        let response = Response::ok(
            3,
            &FsReadResult {
                content: "hello\n".to_owned(),
            },
        )
        .expect("result should serialize");
        let json = serde_json::to_string(&response).expect("response should serialize");
        assert_eq!(json, r#"{"v":1,"id":3,"result":{"content":"hello\n"}}"#);
        assert_eq!(
            serde_json::from_str::<Response>(&json).expect("response should parse"),
            response
        );
        assert!(response.error().is_none());
        let result: FsReadResult = serde_json::from_value(
            response
                .result()
                .expect("response should carry a result")
                .clone(),
        )
        .expect("result should parse as the method's result type");
        assert_eq!(result.content, "hello\n");
    }

    #[test]
    fn an_error_response_serializes_to_the_documented_json() {
        let response = Response::err(
            4,
            ResponseError::new(ErrorCode::Unauthorized, "token did not match"),
        );
        let json = serde_json::to_string(&response).expect("response should serialize");
        assert_eq!(
            json,
            r#"{"v":1,"id":4,"error":{"code":"unauthorized","message":"token did not match"}}"#
        );
        assert_eq!(
            serde_json::from_str::<Response>(&json).expect("response should parse"),
            response
        );
        assert!(response.result().is_none());
        assert_eq!(
            response
                .error()
                .expect("response should carry an error")
                .code,
            ErrorCode::Unauthorized
        );
    }

    #[test]
    fn every_result_type_roundtrips_through_a_response() {
        let responses = [
            Response::ok(
                0,
                &AuthResult {
                    protocol_version: PROTOCOL_VERSION,
                },
            ),
            Response::ok(
                1,
                &FsListResult {
                    entries: vec![DirEntry {
                        name: "notes.md".to_owned(),
                        kind: EntryKind::File,
                    }],
                },
            ),
            Response::ok(
                2,
                &FsReadResult {
                    content: "hello\n".to_owned(),
                },
            ),
            Response::ok(3, &Ack {}),
            Response::ok(4, &FsWatchResult { watch_id: 7 }),
        ];
        for response in responses {
            let response = response.expect("result should serialize");
            let json = serde_json::to_string(&response).expect("response should serialize");
            assert_eq!(
                serde_json::from_str::<Response>(&json).expect("response should parse"),
                response
            );
        }
    }

    #[test]
    fn a_change_push_serializes_to_the_documented_json() {
        let push = ServerPush::new(Event::FsChanged(FsChangedParams {
            watch_id: 7,
            path: "/tmp/notes.md".to_owned(),
            kind: FsChangeKind::Removed,
        }));
        let json = serde_json::to_string(&push).expect("push should serialize");
        assert_eq!(
            json,
            r#"{"v":1,"event":"fs.changed","params":{"watch_id":7,"path":"/tmp/notes.md","kind":"removed"}}"#
        );
        assert_eq!(
            serde_json::from_str::<ServerPush>(&json).expect("push should parse"),
            push
        );
    }

    #[test]
    fn a_message_of_another_version_parses_but_is_not_supported() {
        let request: Request = serde_json::from_str(
            r#"{"v":2,"id":1,"method":"fs.read","params":{"path":"/tmp/notes.md"}}"#,
        )
        .expect("request should parse");
        assert_eq!(request.version, 2);
        assert!(!is_supported_version(request.version));
        assert!(is_supported_version(PROTOCOL_VERSION));
    }
}
