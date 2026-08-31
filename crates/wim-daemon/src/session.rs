//! One connection: the token it opens with, the requests that follow it, and the watches it holds.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::fs;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::{Instant, timeout_at};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use wim_protocol::{
    Ack, AuthResult, DirEntry, EntryKind, ErrorCode, FsListParams, FsListResult, FsReadParams,
    FsReadResult, FsUnwatchParams, FsWatchParams, FsWatchResult, FsWriteParams, Method,
    PROTOCOL_VERSION, Request, Response, ResponseError, is_supported_version,
};

use crate::watch::Watches;
use crate::{Shared, io_error};

/// The half of a connection the daemon reads, once the half it writes has been split off.
type Incoming = SplitStream<WebSocketStream<TcpStream>>;

/// How long a connection that has not presented the token has, handshake and `auth` together.
///
/// A client that means to work sends its `auth` as soon as the socket is up, so this is far more
/// than one needs even on a machine under load; what it is short for is a peer that never presents
/// the token at all. The accept loop spawns a task for every socket, so such a peer would
/// otherwise hold a task and a file descriptor for as long as it liked and could take enough of
/// them to keep clients that do have the token from connecting.
const UNAUTHENTICATED_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves one client until it goes away, or until it fails to present the token.
pub(crate) async fn serve(stream: TcpStream, shared: Arc<Shared>) -> Result<(), WebSocketError> {
    // The handshake and the `auth` that has to follow it share one deadline, so that what a peer
    // which never authenticates can hold is bounded however it stalls. Once the token is in, the
    // connection is a client's to keep for as long as it lives.
    let deadline = Instant::now() + UNAUTHENTICATED_TIMEOUT;
    let websocket = timeout_at(deadline, tokio_tungstenite::accept_async(stream))
        .await
        .map_err(|_| timed_out("the handshake did not finish in time"))??;
    let (mut sink, mut incoming) = websocket.split();
    // Writing is a task of its own so that a watch can push a change while the reading half is
    // waiting on the client's next request. The outbox is unbounded because what fills it is the
    // watcher's callback, which the file system backend runs on a thread of its own that must not
    // be left waiting on a client that is slow to read.
    let (outgoing, mut outbox) = mpsc::unbounded_channel();
    let sending = tokio::spawn(async move {
        while let Some(message) = outbox.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
        // The client may already be gone, and there is nothing to do about it if it is.
        let _ = sink.close().await;
    });
    let mut session = Session {
        shared,
        authenticated: false,
        outgoing,
        watches: Watches::default(),
    };
    let outcome = session.receive(&mut incoming, deadline).await;
    // Dropping the session drops the watches it holds along with the sending half of the outbox,
    // which is what ends the writing task.
    drop(session);
    let _ = sending.await;
    outcome
}

/// What one connection is: what it may ask for, where its messages go out, and what it watches.
struct Session {
    shared: Arc<Shared>,
    /// Whether the connection opened with an `auth` that matched.
    authenticated: bool,
    /// Where responses and `fs.changed` pushes are handed to the writing task.
    outgoing: UnboundedSender<Message>,
    /// The watches this connection asked for, which go away with it.
    watches: Watches,
}

impl Session {
    /// Answers what the client sends until it stops sending.
    ///
    /// `deadline` is when a connection that has not authenticated yet is let go. It bounds every
    /// read taken before the token is in rather than only the first one, because the frames this
    /// daemon passes over — a binary frame, a ping the library answers — leave the connection
    /// waiting for another message without having said anything.
    async fn receive(
        &mut self,
        incoming: &mut Incoming,
        deadline: Instant,
    ) -> Result<(), WebSocketError> {
        loop {
            let message = if self.authenticated {
                incoming.next().await
            } else {
                timeout_at(deadline, incoming.next())
                    .await
                    .map_err(|_| timed_out("the connection did not present the token in time"))?
            };
            let Some(message) = message else { break };
            let text = match message? {
                Message::Text(text) => text,
                Message::Close(_) => break,
                // The protocol travels in text frames; Ping and Pong are answered by the library,
                // and a binary frame is nothing this daemon has anything to say about.
                _ => continue,
            };
            let (id, outcome) = self.answer(text.as_str()).await;
            let response = match outcome {
                Ok(result) => Response::ok(id, &result).expect("a value should serialize"),
                Err(error) => Response::err(id, error),
            };
            let response = serde_json::to_string(&response).expect("a response should serialize");
            if self.outgoing.send(Message::text(response)).is_err() {
                // The writing task is gone, which is the connection being over.
                break;
            }
            if !self.authenticated {
                // The first message of a connection has to be an `auth` that matches. Anything
                // else is answered, and then the connection is dropped rather than given another
                // try (documents/adr/0001-daemon-fs-provider.md).
                break;
            }
        }
        Ok(())
    }

    /// Carries out what one message asks for.
    ///
    /// The id comes back alongside the outcome because a response carries it even when the message
    /// it answers did not parse as a request.
    async fn answer(&mut self, text: &str) -> (u64, Result<Value, ResponseError>) {
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
        // The id is read before the rest, so that a message the daemon cannot make sense of is
        // still answered under the id the client is waiting on.
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
                self.authenticated = params.token == self.shared.token;
                if self.authenticated {
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
            _ if !self.authenticated => Err(ResponseError::new(
                ErrorCode::Unauthorized,
                "the first message of a connection has to be auth",
            )),
            Method::FsList(params) => list(&self.shared, params).await,
            Method::FsRead(params) => read(&self.shared, params).await,
            Method::FsWrite(params) => write(&self.shared, params).await,
            Method::FsWatch(params) => self.watch(params).await,
            Method::FsUnwatch(params) => self.unwatch(params),
        };
        (id, outcome)
    }

    /// Starts reporting changes under a path, and names the watch its pushes carry.
    ///
    /// A path that is not there yet cannot be watched: the file system is asked what it is here,
    /// so that a missing path is reported as missing rather than as a watch that says nothing.
    async fn watch(&mut self, params: FsWatchParams) -> Result<Value, ResponseError> {
        let path = self.shared.root.resolve(&params.path).await?;
        let directory = fs::metadata(&path)
            .await
            .map_err(|error| io_error(&params.path, error))?
            .is_dir();
        let watch_id = self.watches.start(
            &params.path,
            &path,
            params.recursive,
            directory,
            &self.outgoing,
        )?;
        serialized(&FsWatchResult { watch_id })
    }

    /// Drops a watch, whether or not it is one this connection still holds.
    fn unwatch(&mut self, params: FsUnwatchParams) -> Result<Value, ResponseError> {
        self.watches.stop(params.watch_id);
        serialized(&Ack {})
    }
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
/// the same file (`documents/adr/0001-daemon-fs-provider.md`). What makes the last write the one
/// that wins rather than the last few bytes of each is that the content is staged in a file of its
/// own and renamed over the destination: a write that fails partway leaves what was there before,
/// and two connections writing the same path at the same time leave one of the two whole instead
/// of one's opening followed by the other's tail.
async fn write(shared: &Shared, params: FsWriteParams) -> Result<Value, ResponseError> {
    let path = shared.root.resolve(&params.path).await?;
    if path == shared.root.path() {
        // The root is a directory and no file to write, and staging beside it would put the
        // staged file outside the directory this daemon serves.
        return Err(ResponseError::new(
            ErrorCode::Io,
            format!("{}: is the directory this daemon serves", params.path),
        ));
    }
    let staged = staging_path(&path);
    let replaced = match fs::write(&staged, &params.content).await {
        Ok(()) => fs::rename(&staged, &path).await,
        Err(error) => Err(error),
    };
    if let Err(error) = replaced {
        // Whatever went wrong, the staged file is not something to leave behind; if it cannot be
        // taken away either, the error worth reporting is still the first one.
        let _ = fs::remove_file(&staged).await;
        return Err(io_error(&params.path, error));
    }
    serialized(&Ack {})
}

/// Where the content of a write is staged before it replaces `path`.
///
/// Beside the destination, so that the rename that follows stays on the file system the
/// destination is on, and hidden, so that a listing taken mid-write reads as a directory nothing
/// is being written in.
///
/// Not idempotent, and has to be: two writes of the same path — from two connections here, or from
/// another daemon over the same directory — must not stage in one file, which is what the process
/// id and the counter apart from it are for.
fn staging_path(path: &Path) -> PathBuf {
    static STAGED: AtomicU64 = AtomicU64::new(0);

    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let staged = STAGED.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.wim-{}-{staged}", std::process::id()))
}

/// A deadline that ran out, as the error the connection it was on ends with.
fn timed_out(what: &str) -> WebSocketError {
    WebSocketError::Io(io::Error::new(io::ErrorKind::TimedOut, what.to_owned()))
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
