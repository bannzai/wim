//! One connection: the token it opens with, and the requests that follow it.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::fs;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use wim_protocol::{
    Ack, AuthResult, DirEntry, EntryKind, ErrorCode, FsListParams, FsListResult, FsReadParams,
    FsReadResult, FsWriteParams, Method, PROTOCOL_VERSION, Request, Response, ResponseError,
    is_supported_version,
};

use crate::{Shared, io_error};

/// The code the daemon answers a method it does not serve yet with.
///
/// `fs.watch` and `fs.unwatch` are the daemon's own methods, so neither `not_found`, which is
/// about a path, nor `invalid_request`, which is about a message that does not fit, says what
/// happened. The protocol keeps a code a build does not know as [`ErrorCode::Other`], which is
/// how a client of today reads this one.
const NOT_IMPLEMENTED: &str = "not_implemented";

/// Serves one client until it goes away, or until it fails to present the token.
pub(crate) async fn serve(stream: TcpStream, shared: Arc<Shared>) -> Result<(), WebSocketError> {
    let mut websocket = tokio_tungstenite::accept_async(stream).await?;
    let mut authenticated = false;
    while let Some(message) = websocket.next().await {
        let text = match message? {
            Message::Text(text) => text,
            Message::Close(_) => break,
            // The protocol travels in text frames; Ping and Pong are answered by the library, and
            // a binary frame is nothing this daemon has anything to say about.
            _ => continue,
        };
        let (id, outcome) = answer(text.as_str(), &shared, &mut authenticated).await;
        let response = match outcome {
            Ok(result) => Response::ok(id, &result).expect("a value should serialize"),
            Err(error) => Response::err(id, error),
        };
        let response = serde_json::to_string(&response).expect("a response should serialize");
        websocket.send(Message::text(response)).await?;
        if !authenticated {
            // The first message of a connection has to be an `auth` that matches. Anything else
            // is answered, and then the connection is dropped rather than given another try
            // (documents/adr/0001-daemon-fs-provider.md).
            break;
        }
    }
    // The client may already be gone, and there is nothing to do about it if it is.
    let _ = websocket.close(None).await;
    Ok(())
}

/// Carries out what one message asks for.
///
/// The id comes back alongside the outcome because a response carries it even when the message it
/// answers did not parse as a request.
async fn answer(
    text: &str,
    shared: &Shared,
    authenticated: &mut bool,
) -> (u64, Result<Value, ResponseError>) {
    let raw: Value = match serde_json::from_str(text) {
        Ok(raw) => raw,
        Err(error) => {
            let message = format!("the message is not JSON: {error}");
            return (
                0,
                Err(ResponseError::new(ErrorCode::InvalidRequest, message)),
            );
        }
    };
    // The id is read before the rest, so that a message the daemon cannot make sense of is still
    // answered under the id the client is waiting on.
    let id = raw.get("id").and_then(Value::as_u64).unwrap_or(0);
    if let Some(version) = raw.get("v").and_then(Value::as_u64)
        && !u32::try_from(version).is_ok_and(is_supported_version)
    {
        let message =
            format!("this daemon speaks protocol version {PROTOCOL_VERSION}, not {version}");
        return (
            id,
            Err(ResponseError::new(ErrorCode::UnsupportedVersion, message)),
        );
    }
    let request: Request = match serde_json::from_value(raw) {
        Ok(request) => request,
        Err(error) => {
            let message = format!("the message is not a request this daemon serves: {error}");
            return (
                id,
                Err(ResponseError::new(ErrorCode::InvalidRequest, message)),
            );
        }
    };
    let outcome = match request.method {
        Method::Auth(params) => {
            *authenticated = params.token == shared.token;
            if *authenticated {
                serialized(&AuthResult {
                    protocol_version: PROTOCOL_VERSION,
                })
            } else {
                Err(ResponseError::new(
                    ErrorCode::Unauthorized,
                    "the token does not match the one this daemon printed",
                ))
            }
        }
        _ if !*authenticated => Err(ResponseError::new(
            ErrorCode::Unauthorized,
            "the first message of a connection has to be auth",
        )),
        Method::FsList(params) => list(shared, params).await,
        Method::FsRead(params) => read(shared, params).await,
        Method::FsWrite(params) => write(shared, params).await,
        Method::FsWatch(_) => Err(not_implemented("fs.watch")),
        Method::FsUnwatch(_) => Err(not_implemented("fs.unwatch")),
    };
    (id, outcome)
}

/// Lists the direct children of a directory, symlinks reported rather than followed.
async fn list(shared: &Shared, params: FsListParams) -> Result<Value, ResponseError> {
    let path = shared.root.resolve(&params.path).await?;
    let mut reader = fs::read_dir(&path)
        .await
        .map_err(|error| io_error(&params.path, error))?;
    let mut entries = Vec::new();
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|error| io_error(&params.path, error))?
    {
        let kind = entry
            .file_type()
            .await
            .map_err(|error| io_error(&params.path, error))?;
        entries.push(DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind: if kind.is_symlink() {
                EntryKind::Symlink
            } else if kind.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
        });
    }
    serialized(&FsListResult { entries })
}

/// Reads a whole file. A file that is not UTF-8 is an error rather than bytes, because the
/// editing core on the other side works on text.
async fn read(shared: &Shared, params: FsReadParams) -> Result<Value, ResponseError> {
    let path = shared.root.resolve(&params.path).await?;
    let content = fs::read_to_string(&path)
        .await
        .map_err(|error| io_error(&params.path, error))?;
    serialized(&FsReadResult { content })
}

/// Writes a whole file, creating it when it is not there.
///
/// The write is last-write-wins and replaces everything, so writing the same content twice leaves
/// the same file (`documents/adr/0001-daemon-fs-provider.md`).
async fn write(shared: &Shared, params: FsWriteParams) -> Result<Value, ResponseError> {
    let path = shared.root.resolve(&params.path).await?;
    fs::write(&path, &params.content)
        .await
        .map_err(|error| io_error(&params.path, error))?;
    serialized(&Ack {})
}

/// The result of a method, as the value a response carries.
fn serialized<T: Serialize>(result: &T) -> Result<Value, ResponseError> {
    serde_json::to_value(result).map_err(|error| {
        ResponseError::new(
            ErrorCode::Internal,
            format!("the result did not serialize: {error}"),
        )
    })
}

/// What a method the daemon has yet to serve is answered with.
fn not_implemented(method: &str) -> ResponseError {
    ResponseError::new(
        ErrorCode::Other(NOT_IMPLEMENTED.to_owned()),
        format!("{method} is not served yet"),
    )
}
