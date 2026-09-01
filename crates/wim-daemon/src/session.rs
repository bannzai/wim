//! One connection: the token it opens with, the requests that follow it, and the watches it holds.

use std::io;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, File, OpenOptions};
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::fs;
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::sync::mpsc::{self, Sender};
use tokio::time::{Instant, timeout_at};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use wim_protocol::{
    Ack, AuthResult, DirEntry, EntryKind, ErrorCode, FsListParams, FsListResult, FsReadParams,
    FsReadResult, FsUnwatchParams, FsWatchParams, FsWatchResult, FsWriteParams, Method,
    PROTOCOL_VERSION, Response, ResponseError, read_request,
};

use crate::root::{RESERVED_PREFIX, confined_error, is_reserved};
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

/// How many random bytes a staging name is made of. 64 bits, so that a process already on the
/// machine cannot guess the name the next write will stage under and leave something of its own
/// there, and short enough that the name it makes is nowhere near what a file system allows a name
/// to be.
const STAGING_BYTES: usize = 8;

/// How many staging names one write tries before it gives up.
///
/// A name is taken only when the random bytes of two staged files match or when something is
/// planting files at these names, and neither is a reason to keep trying: at 64 random bits a
/// second name that collides is not something a client will see, and a directory being planted in
/// is a write that should fail rather than spin.
const STAGING_ATTEMPTS: usize = 4;

/// How many messages one connection's outbox holds before it is full.
///
/// A push is a few hundred bytes, so this is a few hundred kilobytes per connection: small enough
/// to be a bound worth having, and deep enough to hold the burst a recursive watch sees when a
/// build runs under it while a client that is reading catches up
/// (`documents/adr/0002-daemon-watch-and-staging-robustness.md`).
const OUTBOX_CAPACITY: usize = 1024;

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
    // waiting on the client's next request. The outbox is bounded, and the two things that fill it
    // meet that bound differently: a response waits, which is the request loop taking the pace of
    // the client reading it, and a watch's push cannot wait — the backend's callback thread is not
    // the client's to hold — so it ends the connection instead.
    let (outgoing, mut outbox) = mpsc::channel(OUTBOX_CAPACITY);
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
        overflowed: Arc::new(Notify::new()),
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
    outgoing: Sender<Message>,
    /// Raised when a watch had a change to push and no room to push it in, which is this
    /// connection's last word: what it would report next has already been lost.
    overflowed: Arc<Notify>,
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
    ///
    /// A watch that could not push what it saw ends the loop wherever it is waiting, because from
    /// there on the client would be reading a directory it is no longer being told about.
    async fn receive(
        &mut self,
        incoming: &mut Incoming,
        deadline: Instant,
    ) -> Result<(), WebSocketError> {
        loop {
            let message = tokio::select! {
                biased;
                () = self.overflowed.notified() => break,
                message = next_message(incoming, self.authenticated, deadline) => message?,
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
            if self.outgoing.send(Message::text(response)).await.is_err() {
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
    /// it answers did not parse as a request. Which id that is, and what a message of a version
    /// this daemon does not speak is answered with, are the protocol's own
    /// ([`wim_protocol::read_request`]) rather than this daemon's reading of the JSON.
    async fn answer(&mut self, text: &str) -> (u64, Result<Value, ResponseError>) {
        let request = match read_request(text) {
            Ok(request) => request,
            Err(rejected) => return (rejected.id, Err(rejected.error)),
        };
        let id = request.id;
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
        let path = self.shared.root.resolve_lexically(&params.path)?;
        // `symlink_metadata`, so that a link is watched as the entry it is in the directory holding
        // it: what a watch on that name reports is the name being replaced or removed, and not what
        // happens to the file it points at
        // (`documents/adr/0002-daemon-watch-and-staging-robustness.md`).
        let directory = fs::symlink_metadata(&path)
            .await
            .map_err(|error| io_error(&params.path, error))?
            .is_dir();
        let watch_id = self
            .watches
            .start(
                &params.path,
                &path,
                params.recursive,
                directory,
                &self.outgoing,
                &self.overflowed,
            )
            .await?;
        serialized(&FsWatchResult { watch_id })
    }

    /// Drops a watch, whether or not it is one this connection still holds.
    fn unwatch(&mut self, params: FsUnwatchParams) -> Result<Value, ResponseError> {
        self.watches.stop(params.watch_id);
        serialized(&Ack {})
    }
}

/// Lists the direct children of a directory, symlinks reported rather than followed.
///
/// The daemon's own working files are not among them: a listing taken while another connection is
/// writing would otherwise name a staged file that is gone by the time the client asks about it.
async fn list(shared: &Shared, params: FsListParams) -> Result<Value, ResponseError> {
    let path = shared.root.relative(&params.path)?;
    let entries = shared
        .root
        .blocking(move |dir| {
            let mut entries = Vec::new();
            for entry in dir.read_dir(&path)? {
                let entry = entry?;
                let name = entry.file_name();
                if is_reserved(&name) {
                    continue;
                }
                // A name that is not UTF-8 is left out rather than reported with the replacement
                // characters `to_string_lossy` puts in its place: what a client composes onto the
                // listed directory's path is the name it is given, and that one names nothing
                // (`crates/wim-protocol/src/fs.rs`).
                let Some(name) = name.to_str().map(str::to_owned) else {
                    continue;
                };
                let kind = entry.file_type()?;
                entries.push(DirEntry {
                    name,
                    kind: if kind.is_symlink() {
                        EntryKind::Symlink
                    } else if kind.is_dir() {
                        EntryKind::Directory
                    } else if kind.is_file() {
                        EntryKind::File
                    } else {
                        // A socket, a FIFO, a device. Named for what it is rather than called a
                        // file, which would have a client open it and read it whole.
                        EntryKind::Other
                    },
                });
            }
            Ok(entries)
        })
        .await
        .map_err(|error| confined_error(&params.path, error))?;
    serialized(&FsListResult { entries })
}

/// Reads a whole file. A file that is not UTF-8 is an error rather than bytes, because the
/// editing core on the other side works on text.
async fn read(shared: &Shared, params: FsReadParams) -> Result<Value, ResponseError> {
    let path = shared.root.relative(&params.path)?;
    let content = shared
        .root
        .blocking(move |dir| {
            let mut content = String::new();
            dir.open(&path)?.read_to_string(&mut content)?;
            Ok(content)
        })
        .await
        .map_err(|error| confined_error(&params.path, error))?;
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
///
/// Staging asks for a directory one may create in, which a file one may write to does not: a file
/// that is `0644` in a directory that is `0555` was writable before writes were staged. That one
/// case falls back to writing in place
/// (`documents/adr/0002-daemon-watch-and-staging-robustness.md`).
async fn write(shared: &Shared, params: FsWriteParams) -> Result<Value, ResponseError> {
    let FsWriteParams {
        path: requested,
        content,
    } = params;
    let path = shared.root.relative(&requested)?;
    if path == Path::new(".") {
        // The root is a directory and no file to write, and staging beside it would put the
        // staged file outside the directory this daemon serves.
        return Err(ResponseError::new(
            ErrorCode::Io,
            format!("{requested}: is the directory this daemon serves"),
        ));
    }
    shared
        .root
        .blocking(move |dir| replace(dir, &path, &content))
        .await
        .map_err(|error| confined_error(&requested, error))?;
    serialized(&Ack {})
}

/// Puts `content` at `path` under `dir`, staged and renamed over where that can be done and
/// written over where it cannot.
///
/// What the rename replaces is the name the request gave rather than what that name points at, so
/// a link under the root becomes the file that was written through it. Following it instead would
/// mean resolving the last component here, and a link out of the root would then be a write
/// outside the directory this daemon serves
/// (`documents/adr/0003-daemon-beneath-semantics-with-cap-std.md`).
fn replace(dir: &Dir, path: &Path, content: &str) -> io::Result<()> {
    let staged = match stage(dir, path, content) {
        Ok(staged) => staged,
        Err(error) => {
            // A directory that cannot be staged in is the only reason to write any other way, and
            // a file that is already there the only thing to write that way: a destination that is
            // not there has to be created, which is the permission the directory just refused. A
            // path that left the root is refused the same way, and answers this the same way a
            // path that is not there does: there is nothing to fall back to.
            if error.kind() != io::ErrorKind::PermissionDenied
                || !dir.metadata(path).is_ok_and(|metadata| metadata.is_file())
            {
                return Err(error);
            }
            // The error worth reporting is the one the write actually ended on, so the fallback's
            // own failure replaces the refusal that led here.
            return write_in_place(dir, path, content);
        }
    };
    if let Err(error) = dir.rename(&staged, dir, path) {
        // The staged file is not something to leave behind; if it cannot be taken away either, the
        // error worth reporting is still the first one.
        let _ = dir.remove_file(&staged);
        return Err(error);
    }
    Ok(())
}

/// Writes `content` into the file that is already at `path`, over the bytes that are there.
///
/// What a write costs when it cannot be staged: the file is truncated and filled where the client
/// reads it, so a write that fails partway — or a machine that goes down mid-write — leaves the
/// file neither the old content nor the new one. That is the trade the fallback makes to keep a
/// writable file in an unwritable directory writable at all, and it is what Vim's `backupcopy=yes`
/// trades for the same reason.
///
/// No `create`: the directory this runs for is one that refused a new file, so a destination that
/// is not there is not something this could make: the fallback replaces content and never widens
/// what a write may bring into existence.
///
/// No following of a link at the destination either. This is the one place a write opens the
/// destination rather than a file of its own, and what it does to it is truncate it: a link left
/// at the name between the staging that failed and this open would otherwise be a write emptying
/// whatever it points at. `create_new` is what keeps the staged file from being a link; this is
/// what keeps the destination from being one
/// (`documents/adr/0003-daemon-beneath-semantics-with-cap-std.md`).
fn write_in_place(dir: &Dir, path: &Path, content: &str) -> io::Result<()> {
    let mut file = dir
        .open_with(
            path,
            OpenOptions::new()
                .write(true)
                .truncate(true)
                .follow(FollowSymlinks::No),
        )
        .map_err(|error| refused_a_link(dir, path, error))?;
    file.write_all(content.as_bytes())?;
    file.flush()
}

/// A link refused at the destination, said as what it is; anything else as it came.
///
/// An open that will not follow a link reports one the way the operating system does — on Unix
/// `ELOOP`, whose message is that too many links were followed, when none was. Which of the two
/// happened is worked out by looking at the name afterwards rather than at the error, because the
/// error says the same thing on one link as on a hundred and what it is called is not the same on
/// every platform. Looking afterwards is safe where looking beforehand would not be: the write has
/// already not happened, and what is at the name now cannot make this open have followed a link.
///
/// `AlreadyExists` is what a link at a staging name comes back as, and this is the same thing at
/// the other end of the same write: something that is not this daemon's to write through is at the
/// name it was going to write.
fn refused_a_link(dir: &Dir, path: &Path, error: io::Error) -> io::Error {
    if !dir
        .symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return error;
    }
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        "is a link, and a write that cannot be staged does not write through one",
    )
}

/// Stages `content` in a file of its own beside `destination`, and names that file for the rename
/// that replaces the destination with it.
///
/// Beside the destination, so that the rename stays on the file system the destination is on. A
/// name that is already taken is a name to try again under rather than a name to open: opening it
/// would be following whatever is there — a symlink another process on the machine left, pointing
/// at a file outside the root — and truncating what it points at.
fn stage(dir: &Dir, destination: &Path, content: &str) -> io::Result<PathBuf> {
    for _ in 0..STAGING_ATTEMPTS {
        let staged = destination.with_file_name(staging_name());
        let mut file = match create_staged(dir, &staged) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = fill(dir, &mut file, destination, content) {
            drop(file);
            let _ = dir.remove_file(&staged);
            return Err(error);
        }
        return Ok(staged);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "every name the content could be staged under was taken",
    ))
}

/// Opens `staged` as a file this write is the only one to have written to.
///
/// `create_new` is what makes it that file rather than whatever the name already holds: it fails
/// on a path that is there, symlinks included, so a link left at the name is a write that fails
/// instead of a write through it.
fn create_staged(dir: &Dir, staged: &Path) -> io::Result<File> {
    dir.open_with(staged, OpenOptions::new().write(true).create_new(true))
}

/// Puts the content of a write in the staged file, as the file that is about to be `destination`.
///
/// The rename replaces the destination's inode with this one, so the permissions of a destination
/// that is already there are carried over: without them a `0755` script saved through the daemon
/// would come back `0644`, which is what the process umask made the staged file. A destination
/// that is not there yet is a file being created, and keeps the permissions the umask gave it.
fn fill(dir: &Dir, file: &mut File, destination: &Path, content: &str) -> io::Result<()> {
    file.write_all(content.as_bytes())?;
    file.flush()?;
    if let Ok(metadata) = dir.metadata(destination) {
        file.set_permissions(metadata.permissions())?;
    }
    Ok(())
}

/// A name to stage one write under.
///
/// Hidden, so that a listing taken mid-write reads as a directory nothing is being written in, and
/// the same length whatever the destination is called: a name built from the destination's own —
/// which may be the 255 bytes a file system allows — would be longer than a name may be, and the
/// write of a file that could be created before would fail.
///
/// Not idempotent, and has to be: two writes of the same path — from two connections here, or from
/// another daemon over the same directory — must not stage in one file.
fn staging_name() -> String {
    let mut bytes = [0u8; STAGING_BYTES];
    getrandom::fill(&mut bytes).expect("the operating system should have a random generator");
    let staged: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{RESERVED_PREFIX}{staged}")
}

/// The next message of a connection, under the deadline one that has not authenticated is held to.
async fn next_message(
    incoming: &mut Incoming,
    authenticated: bool,
    deadline: Instant,
) -> Result<Option<Result<Message, WebSocketError>>, WebSocketError> {
    if authenticated {
        Ok(incoming.next().await)
    } else {
        timeout_at(deadline, incoming.next())
            .await
            .map_err(|_| timed_out("the connection did not present the token in time"))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    use crate::Root;

    /// What the write path reads, over a root with `secret.md` next to it rather than in it.
    async fn shared() -> (TempDir, Shared) {
        let directory = TempDir::new().expect("a temporary directory should be available");
        let path = directory.path().join("root");
        std::fs::create_dir(&path).expect("the root should be created");
        std::fs::write(directory.path().join("secret.md"), "secret\n")
            .expect("the file should be written");
        let root = Root::new(&path).await.expect("the root should resolve");
        (
            directory,
            Shared {
                root,
                token: String::new(),
            },
        )
    }

    /// The write of `content` to `path`, as the method a client's request reaches.
    async fn write_through(
        shared: &Shared,
        path: &str,
        content: &str,
    ) -> Result<Value, ResponseError> {
        write(
            shared,
            FsWriteParams {
                path: path.to_owned(),
                content: content.to_owned(),
            },
        )
        .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_left_at_a_staging_name_is_not_written_through() {
        let (directory, shared) = shared().await;
        let outside = directory.path().join("secret.md");
        let staged = shared.root.path().join(".wim-0123456789abcdef");
        std::os::unix::fs::symlink(&outside, &staged).expect("the link should be created");

        let error = shared
            .root
            .blocking(|dir| create_staged(dir, Path::new(".wim-0123456789abcdef")))
            .await
            .expect_err("a name that is already taken should not open");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(&outside).expect("the file should be readable"),
            "secret\n",
            "what the link points at is left as it was"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_write_over_a_file_leaves_it_with_the_permissions_it_had() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, shared) = shared().await;
        let path = shared.root.path().join("run.sh");
        std::fs::write(&path, "#!/bin/sh\n").expect("the file should be written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("the permissions should be set");

        write_through(&shared, "run.sh", "#!/bin/sh\necho hello\n")
            .await
            .expect("the write should go through");

        let mode = std::fs::metadata(&path)
            .expect("the file should be there")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "{mode:o}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("the file should be readable"),
            "#!/bin/sh\necho hello\n"
        );
    }

    /// Whether `directory` refuses a file being created in it, which is what a write falls back
    /// out of staging for.
    ///
    /// A process running as root is refused nothing whatever the mode says, so a test of the
    /// fallback has nothing to test where this is false and says so rather than failing.
    #[cfg(unix)]
    fn refuses_creation(directory: &Path) -> bool {
        let probe = directory.join("probe");
        match std::fs::File::create(&probe) {
            Ok(_) => {
                std::fs::remove_file(&probe).expect("the probe should be removed");
                false
            }
            Err(error) => error.kind() == io::ErrorKind::PermissionDenied,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_write_over_a_file_in_a_directory_that_refuses_staging_goes_through_in_place() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, shared) = shared().await;
        let path = shared.root.path().join("notes.md");
        std::fs::write(&path, "before\n").expect("the file should be written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("the permissions of the file should be set");
        std::fs::set_permissions(shared.root.path(), std::fs::Permissions::from_mode(0o555))
            .expect("the permissions of the directory should be set");

        let outcome = if refuses_creation(shared.root.path()) {
            Some(write_through(&shared, "notes.md", "after\n").await)
        } else {
            None
        };

        // Put back before anything may panic, so that the temporary directory can be taken away.
        std::fs::set_permissions(shared.root.path(), std::fs::Permissions::from_mode(0o755))
            .expect("the permissions of the directory should be put back");
        let Some(outcome) = outcome else {
            return;
        };
        outcome.expect("the write should go through");
        assert_eq!(
            std::fs::read_to_string(&path).expect("the file should be readable"),
            "after\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_write_of_a_file_that_is_not_there_fails_in_a_directory_that_refuses_staging() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, shared) = shared().await;
        let path = shared.root.path().join("notes.md");
        std::fs::set_permissions(shared.root.path(), std::fs::Permissions::from_mode(0o555))
            .expect("the permissions of the directory should be set");

        let outcome = if refuses_creation(shared.root.path()) {
            Some(write_through(&shared, "notes.md", "hello\n").await)
        } else {
            None
        };

        std::fs::set_permissions(shared.root.path(), std::fs::Permissions::from_mode(0o755))
            .expect("the permissions of the directory should be put back");
        let Some(outcome) = outcome else {
            return;
        };
        outcome.expect_err("a file that is not there should not be created in place");
        assert!(!path.exists(), "the file is not brought into existence");
    }

    /// The one open a write makes on the destination itself is the fallback's, and it truncates
    /// what it opens. A link planted at the name after the staging that failed would empty what it
    /// points at if that open followed it, and this is what fixes that it does not
    /// (`documents/adr/0003-daemon-beneath-semantics-with-cap-std.md`).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_write_that_falls_back_does_not_write_through_a_link_left_at_the_destination() {
        use std::os::unix::fs::PermissionsExt;

        let (directory, shared) = shared().await;
        let outside = directory.path().join("secret.md");
        // The link stands where the write is headed, and what it points at is outside the root:
        // the file a fallback that followed it would truncate.
        std::os::unix::fs::symlink(&outside, shared.root.path().join("notes.md"))
            .expect("the link should be created");
        std::fs::set_permissions(shared.root.path(), std::fs::Permissions::from_mode(0o555))
            .expect("the permissions of the directory should be set");

        let outcome = if refuses_creation(shared.root.path()) {
            Some(write_through(&shared, "notes.md", "planted\n").await)
        } else {
            None
        };

        std::fs::set_permissions(shared.root.path(), std::fs::Permissions::from_mode(0o755))
            .expect("the permissions of the directory should be put back");
        let Some(outcome) = outcome else {
            return;
        };
        outcome.expect_err("a write should not go through a link at the destination");
        assert_eq!(
            std::fs::read_to_string(&outside).expect("the file should be readable"),
            "secret\n",
            "what the link points at is left as it was"
        );
        assert!(
            std::fs::symlink_metadata(shared.root.path().join("notes.md"))
                .expect("the link should be there")
                .file_type()
                .is_symlink(),
            "the link itself is left as it was"
        );
    }

    /// The same refusal where the link points back into the root: what is refused is writing
    /// through a link, not leaving the root, so this does not depend on where the link goes.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_write_that_falls_back_does_not_write_through_a_link_into_the_root_either() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, shared) = shared().await;
        let target = shared.root.path().join("target.md");
        std::fs::write(&target, "target\n").expect("the file should be written");
        std::os::unix::fs::symlink("target.md", shared.root.path().join("notes.md"))
            .expect("the link should be created");
        std::fs::set_permissions(shared.root.path(), std::fs::Permissions::from_mode(0o555))
            .expect("the permissions of the directory should be set");

        let outcome = if refuses_creation(shared.root.path()) {
            Some(write_through(&shared, "notes.md", "planted\n").await)
        } else {
            None
        };

        std::fs::set_permissions(shared.root.path(), std::fs::Permissions::from_mode(0o755))
            .expect("the permissions of the directory should be put back");
        let Some(outcome) = outcome else {
            return;
        };
        let error = outcome.expect_err("a write should not go through a link at the destination");
        assert!(
            error.message.contains("does not write through one"),
            "the refusal says what was refused rather than reporting a loop: {}",
            error.message
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("the file should be readable"),
            "target\n",
            "what the link points at is left as it was"
        );
    }

    #[tokio::test]
    async fn a_write_to_a_name_as_long_as_the_file_system_allows_goes_through() {
        let (_directory, shared) = shared().await;
        // The longest name a typical file system takes, which was writable before the content was
        // staged in a file beside it and has to stay writable now that it is.
        let name = "n".repeat(255);

        write_through(&shared, &name, "hello\n")
            .await
            .expect("the write should go through");

        assert_eq!(
            std::fs::read_to_string(shared.root.path().join(&name))
                .expect("the file should be readable"),
            "hello\n"
        );
    }

    /// A listing reports a link as the entry it is, whether or not what it points at is something
    /// this daemon serves: what the entry is has to come from the directory rather than from
    /// following the name, which is the one thing a directory handle's entries could have changed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_listing_reports_a_link_as_a_link_wherever_it_points() {
        let (directory, shared) = shared().await;
        std::fs::write(shared.root.path().join("notes.md"), "hello\n")
            .expect("the file should be written");
        std::os::unix::fs::symlink("notes.md", shared.root.path().join("inside.md"))
            .expect("the link should be created");
        std::os::unix::fs::symlink(
            directory.path().join("secret.md"),
            shared.root.path().join("outside.md"),
        )
        .expect("the link should be created");

        let result = list(
            &shared,
            FsListParams {
                path: ".".to_owned(),
            },
        )
        .await
        .expect("the root should be listed");

        let listed: FsListResult =
            serde_json::from_value(result).expect("the result should be a listing");
        let mut kinds: Vec<(String, EntryKind)> = listed
            .entries
            .into_iter()
            .map(|entry| (entry.name, entry.kind))
            .collect();
        kinds.sort_by(|one, other| one.0.cmp(&other.0));
        assert_eq!(
            kinds,
            vec![
                ("inside.md".to_owned(), EntryKind::Symlink),
                ("notes.md".to_owned(), EntryKind::File),
                ("outside.md".to_owned(), EntryKind::Symlink),
            ]
        );
    }

    /// A file in `directory` whose name is not UTF-8, and `None` where the file system will not
    /// take one.
    ///
    /// APFS and HFS+ hold names as UTF-8 and refuse anything else, so a test of what a listing does
    /// with such a name has nothing to test on a macOS machine and says so rather than failing.
    #[cfg(unix)]
    fn write_a_name_that_is_not_utf8(directory: &Path) -> Option<PathBuf> {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        // `0xff` begins no UTF-8 sequence, in a name that is otherwise an ordinary one.
        let path = directory.join(OsStr::from_bytes(b"not-utf8-\xff.md"));
        std::fs::write(&path, "bytes\n").ok().map(|()| path)
    }

    /// A child whose name is not UTF-8 is one no client could name back, so the listing leaves it
    /// out rather than reporting the replacement characters the name reads as
    /// (`crates/wim-protocol/src/fs.rs`).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_listing_leaves_out_a_child_whose_name_is_not_utf8() {
        let (_directory, shared) = shared().await;
        std::fs::write(shared.root.path().join("notes.md"), "hello\n")
            .expect("the file should be written");
        let Some(_path) = write_a_name_that_is_not_utf8(shared.root.path()) else {
            return;
        };

        let result = list(
            &shared,
            FsListParams {
                path: ".".to_owned(),
            },
        )
        .await
        .expect("the root should be listed");

        let listed: FsListResult =
            serde_json::from_value(result).expect("the result should be a listing");
        assert_eq!(
            listed.entries,
            [DirEntry {
                name: "notes.md".to_owned(),
                kind: EntryKind::File,
            }]
        );
    }

    /// The read of `path`, as the method a client's request reaches.
    async fn read_through(shared: &Shared, path: &str) -> Result<String, ResponseError> {
        let result = read(
            shared,
            FsReadParams {
                path: path.to_owned(),
            },
        )
        .await?;
        let read: FsReadResult =
            serde_json::from_value(result).expect("the result should be a read");
        Ok(read.content)
    }

    /// A link out of the root is refused where it is followed rather than before it is: nothing
    /// resolves the path ahead of the open any more, so this is what fixes that the client is told
    /// the same thing it was told when something did
    /// (`documents/adr/0003-daemon-beneath-semantics-with-cap-std.md`).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_through_a_link_that_leaves_the_root_is_refused() {
        let (directory, shared) = shared().await;
        std::os::unix::fs::symlink(
            directory.path().join("secret.md"),
            shared.root.path().join("link.md"),
        )
        .expect("the link should be created");

        let error = read_through(&shared, "link.md")
            .await
            .expect_err("reading through a link out of the root should be refused");

        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert_eq!(
            error.message,
            "link.md: outside the directory this daemon serves"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_through_a_link_that_stays_in_the_root_goes_through() {
        let (_directory, shared) = shared().await;
        std::fs::write(shared.root.path().join("notes.md"), "hello\n")
            .expect("the file should be written");
        // Relative, because what an absolute link target names is a path in the file system the
        // daemon was started in rather than one under the root.
        std::os::unix::fs::symlink("notes.md", shared.root.path().join("link.md"))
            .expect("the link should be created");

        let content = read_through(&shared, "link.md")
            .await
            .expect("a link under the root should be read through");

        assert_eq!(content, "hello\n");
    }

    #[test]
    fn a_staging_name_is_the_same_length_every_time_and_no_two_of_them_match() {
        let one = staging_name();
        let other = staging_name();
        assert_eq!(one.len(), ".wim-".len() + STAGING_BYTES * 2);
        assert_eq!(one.len(), other.len());
        assert_ne!(one, other);
    }
}
