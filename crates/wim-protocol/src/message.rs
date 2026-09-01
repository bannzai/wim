//! The three envelopes that cross the wire, each carrying the protocol version as `v`.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::error::{ErrorCode, ResponseError};
use crate::fs::{
    AuthParams, FsChangedParams, FsListParams, FsReadParams, FsUnwatchParams, FsWatchParams,
    FsWriteParams,
};

/// Version of the protocol these types speak, sent as `v` on every message.
pub const PROTOCOL_VERSION: u32 = 1;

/// The `id` a response carries when it answers a message no request id could be read from.
///
/// Reserved rather than usable: clients number their requests from 1, so that a response under
/// this id is one the client can tell apart from the answer to something it sent. A message whose
/// `id` is missing or is not a number is still answered — the sender is told what was wrong with
/// it rather than left waiting — and this is the id that answer carries.
pub const RESERVED_ID: u64 = 0;

/// Whether a message tagged with `version` can be handled by this build.
///
/// A message of another version still parses, so that the side reading it can answer with
/// [`crate::ErrorCode::UnsupportedVersion`] instead of dropping the connection blind.
pub fn is_supported_version(version: u32) -> bool {
    version == PROTOCOL_VERSION
}

/// The `v` and `id` of a message, read before the rest of it.
///
/// A message cannot be read as a [`Request`] first: a later version of the protocol may name a
/// method this build has never heard of or shape params differently, and that parse would fail
/// before `v` was ever looked at — leaving a message which says which version it speaks answered
/// as one that does not parse at all. So the two fields every message carries are read on their
/// own, and the rest of it only once the version is one this build speaks. [`read_request`] is
/// those two steps in one call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Envelope {
    /// The protocol version the message announced, `None` when it carried none that is a number.
    #[serde(rename = "v", default, deserialize_with = "number_or_none")]
    pub version: Option<u64>,
    /// The id to answer under, [`RESERVED_ID`] when the message carried none that is a number.
    #[serde(default, deserialize_with = "id_or_reserved")]
    pub id: u64,
}

impl Envelope {
    /// The envelope of `message`.
    ///
    /// Never fails, because what an envelope is read for is answering: a message carrying neither
    /// field readably — one that is not even an object — comes back as an envelope that announced
    /// no version, under the reserved id.
    pub fn of(message: &Value) -> Self {
        Self::deserialize(message).unwrap_or_default()
    }

    /// The version the message announced when that is not one this build speaks.
    ///
    /// `None` for a message of this build's own version and for one that announced none: a message
    /// without a version is not one to refuse on its version — what is wrong with it is that it is
    /// not a message of this protocol at all, which reading it as a [`Request`] is what says.
    pub fn unsupported_version(&self) -> Option<u64> {
        self.version
            .filter(|version| !u32::try_from(*version).is_ok_and(is_supported_version))
    }
}

/// A message that is not a [`Request`] this build can carry out, and what to answer it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    /// The id the answer carries, [`RESERVED_ID`] when the message held none that could be read.
    pub id: u64,
    /// What to tell the sender.
    pub error: ResponseError,
}

/// One message read as the request it holds, in the two steps that keep a version this build does
/// not speak from being reported as a message it cannot parse.
///
/// Both steps are here rather than at each call site so that every side of the protocol reads a
/// message the same way, and so that what a message of a later version is answered with is the
/// crate's own contract rather than one daemon's reading of it.
pub fn read_request(text: &str) -> Result<Request, Rejected> {
    let message: Value = match serde_json::from_str(text) {
        Ok(message) => message,
        Err(error) => {
            return Err(Rejected {
                id: RESERVED_ID,
                error: ResponseError::new(
                    ErrorCode::InvalidRequest,
                    format!("the message is not JSON: {error}"),
                ),
            });
        }
    };
    let envelope = Envelope::of(&message);
    if let Some(version) = envelope.unsupported_version() {
        return Err(Rejected {
            id: envelope.id,
            error: ResponseError::new(
                ErrorCode::UnsupportedVersion,
                format!("this build speaks protocol version {PROTOCOL_VERSION}, not {version}"),
            ),
        });
    }
    serde_json::from_value(message).map_err(|error| Rejected {
        id: envelope.id,
        error: ResponseError::new(
            ErrorCode::InvalidRequest,
            format!("the message is not a request this build serves: {error}"),
        ),
    })
}

/// The number a field of an envelope holds, and `None` when it holds anything else.
///
/// Reading an envelope never fails, so a field that is there as something other than a number is
/// read as one that is not there: the message is still one to answer, and answering it is the
/// whole reason the envelope is read.
fn number_or_none<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    Ok(Option::<Value>::deserialize(deserializer)?
        .as_ref()
        .and_then(Value::as_u64))
}

/// The `id` of an envelope, falling back to the id kept for messages that carry none.
fn id_or_reserved<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    Ok(number_or_none(deserializer)?.unwrap_or(RESERVED_ID))
}

/// A client asking the daemon for something: `{"v":1,"id":1,"method":"fs.read","params":{...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Protocol version.
    #[serde(rename = "v")]
    pub version: u32,
    /// Names the response that answers this request. Chosen by the client, and numbered from 1:
    /// [`RESERVED_ID`] is kept for answers that could not be matched to a request at all.
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
    /// The `id` of the request being answered, or [`RESERVED_ID`] when the message this answers
    /// carried no id that could be read.
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
            1,
            Method::Auth(AuthParams {
                token: "s3cret".to_owned(),
            }),
        );
        let json = serde_json::to_string(&request).expect("request should serialize");
        assert_eq!(
            json,
            r#"{"v":1,"id":1,"method":"auth","params":{"token":"s3cret"}}"#
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
            let request = Request::new(id as u64 + 1, method);
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
            let json = serde_json::to_value(Request::new(1, method)).expect("should serialize");
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
                1,
                &AuthResult {
                    protocol_version: PROTOCOL_VERSION,
                },
            ),
            Response::ok(
                2,
                &FsListResult {
                    entries: vec![DirEntry {
                        name: "notes.md".to_owned(),
                        kind: EntryKind::File,
                    }],
                },
            ),
            Response::ok(
                3,
                &FsReadResult {
                    content: "hello\n".to_owned(),
                },
            ),
            Response::ok(4, &Ack {}),
            Response::ok(5, &FsWatchResult { watch_id: 7 }),
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

    #[test]
    fn a_message_of_a_future_version_with_a_method_this_build_has_never_heard_of_is_unsupported_version_and_not_invalid_request()
     {
        // The version is read before the method is: a `Request` built straight from this JSON
        // would fail to deserialize on `method`, which is not the answer a message naming a later
        // version should get.
        let text = r#"{"v":2,"id":9,"method":"fs.teleport","params":{"anything":true}}"#;

        let rejected = read_request(text).expect_err("a message of another version is refused");

        assert_eq!(rejected.id, 9);
        assert_eq!(rejected.error.code, crate::ErrorCode::UnsupportedVersion);
    }

    #[test]
    fn a_message_this_builds_own_version_but_with_an_unknown_method_is_invalid_request() {
        let text = r#"{"v":1,"id":9,"method":"fs.teleport","params":{"anything":true}}"#;

        let rejected = read_request(text).expect_err("an unknown method does not parse");

        assert_eq!(rejected.id, 9);
        assert_eq!(rejected.error.code, crate::ErrorCode::InvalidRequest);
    }

    #[test]
    fn a_well_formed_request_of_this_builds_version_reads_through() {
        let text = r#"{"v":1,"id":3,"method":"fs.read","params":{"path":"/tmp/notes.md"}}"#;

        let request = read_request(text).expect("a well-formed request should read");

        assert_eq!(request.id, 3);
        assert_eq!(
            request.method,
            Method::FsRead(crate::fs::FsReadParams {
                path: "/tmp/notes.md".to_owned(),
            })
        );
    }

    #[test]
    fn a_message_with_no_id_that_can_be_read_is_rejected_under_the_reserved_id() {
        for text in ["{ not json", r#"{"v":1,"method":"fs.explode","params":{}}"#] {
            let rejected = read_request(text).expect_err("the message should not read");
            assert_eq!(rejected.id, RESERVED_ID, "{text}");
        }
    }

    #[test]
    fn an_envelope_reads_the_id_and_version_a_request_does_without_reading_the_rest_of_it() {
        let message: Value =
            serde_json::from_str(r#"{"v":1,"id":5,"method":"fs.read","params":{}}"#)
                .expect("json should parse");
        let envelope = Envelope::of(&message);
        assert_eq!(envelope.version, Some(1));
        assert_eq!(envelope.id, 5);
        assert_eq!(envelope.unsupported_version(), None);
    }

    #[test]
    fn an_envelope_with_no_readable_id_or_version_falls_back_without_failing() {
        let message: Value =
            serde_json::from_str(r#"{"method":"fs.read"}"#).expect("json should parse");
        let envelope = Envelope::of(&message);
        assert_eq!(envelope.version, None);
        assert_eq!(envelope.id, RESERVED_ID);
        assert_eq!(envelope.unsupported_version(), None);
    }
}
