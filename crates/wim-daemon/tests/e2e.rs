//! End-to-end tests over a running daemon.
//!
//! The acceptance test of the daemon is the round trip a real client makes: read a file over the
//! WebSocket, edit it with `wim-core` on this side of the wire — where the buffer lives
//! (`documents/adr/0001-daemon-fs-provider.md`) — write it back, and find the result on disk.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use wim_core::Editor;
use wim_daemon::Daemon;
use wim_protocol::{
    Ack, AuthParams, AuthResult, DirEntry, EntryKind, ErrorCode, Event, FsChangeKind,
    FsChangedParams, FsListParams, FsListResult, FsReadParams, FsReadResult, FsUnwatchParams,
    FsWatchParams, FsWatchResult, FsWriteParams, Method, PROTOCOL_VERSION, Request, Response,
    ResponseError, ServerPush,
};

/// How long a test keeps making a change before it calls the watch broken.
///
/// The backends report asynchronously and none of them promises when, so the wait has to hold for
/// a loaded CI runner; a run where the watch works spends a fraction of it.
const CHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a test waits for one change before making it again.
///
/// A backend is watching only some time after it says it is, and `fs.watch` narrows that window
/// rather than closing it: it probes for up to two seconds
/// (`documents/adr/0002-daemon-watch-and-staging-robustness.md`), and an FSEvents stream was
/// measured taking about four to become live on a loaded macOS machine. One change going
/// unreported is therefore not yet a watch that does not work, and making the change again is what
/// tells the two apart.
const CHANGE_RETRY: Duration = Duration::from_millis(500);

/// How long a test watches for a change that should not come.
///
/// Absence cannot be waited out, so this is a window wide enough that a watch which is still live
/// would have reported the change made in front of it.
const NO_CHANGE_WINDOW: Duration = Duration::from_secs(2);

/// How long a test waits for the daemon to let go of a connection that never presents the token.
///
/// Wider than the deadline the daemon holds such a connection to, so that a test which fails says
/// the connection was never let go rather than that it was let go later than the test measured.
const UNAUTHENTICATED_WINDOW: Duration = Duration::from_secs(30);

/// A daemon serving a directory, with a file outside that directory to reach for.
struct Fixture {
    /// Holds the root and everything outside it, and takes it all away when the test ends.
    directory: TempDir,
    root: PathBuf,
    addr: SocketAddr,
    token: String,
}

impl Fixture {
    /// Starts a daemon over a directory holding `files`, given as `(path, content)` pairs, with
    /// `secret.md` next to that directory rather than in it.
    async fn start(files: &[(&str, &str)]) -> Self {
        let directory = TempDir::new().expect("a temporary directory should be available");
        let root = directory.path().join("root");
        std::fs::create_dir(&root).expect("the root should be created");
        // Resolved, because an absolute path a request names is confined by the root's own name:
        // a temporary directory reached through a link — `/var` on macOS — is a path the daemon
        // does not serve under the name it was made with
        // (`documents/adr/0003-daemon-beneath-semantics-with-cap-std.md`).
        let root = std::fs::canonicalize(&root).expect("the root should resolve");
        std::fs::write(directory.path().join("secret.md"), "secret\n")
            .expect("the file should be written");
        for (path, content) in files {
            let path = root.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the directory should be created");
            }
            std::fs::write(path, content).expect("the file should be written");
        }
        let daemon = Daemon::bind(loopback(), &root)
            .await
            .expect("the daemon should bind");
        let addr = daemon
            .local_addr()
            .expect("the daemon should have an address");
        let token = daemon.token().to_owned();
        // The daemon serves until the test ends and the runtime it was spawned on goes away.
        tokio::spawn(daemon.serve());
        Self {
            directory,
            root,
            addr,
            token,
        }
    }

    /// A path in the directory that holds the root, which is outside what the daemon serves.
    fn outside(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join(name)).expect("the file should be readable")
    }

    /// Writes a file in the root straight to disk, the way a program other than the daemon would.
    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.root.join(name), content).expect("the file should be written");
    }

    /// What is directly under `path`, named and in order, read straight off disk.
    ///
    /// A write stages its content in a file beside the destination, so what a test asks this is
    /// whether the daemon took that file away again.
    fn names(&self, path: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path)
            .expect("the directory should be readable")
            .map(|entry| {
                entry
                    .expect("the entry should be readable")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

/// Loopback and a port the operating system picks, so that tests can run side by side.
fn loopback() -> SocketAddr {
    "127.0.0.1:0"
        .parse()
        .expect("the address should be an address")
}

/// A message the daemon sent, which is a response to a request or a push nothing asked for.
#[derive(Debug)]
enum Incoming {
    Response(Response),
    Push(ServerPush),
}

/// A client of the daemon, playing the part a wim front end plays.
struct Client {
    websocket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    /// Pushes that arrived while a response was being waited on.
    pushes: VecDeque<ServerPush>,
}

impl Client {
    /// Connects without saying anything yet.
    async fn connect(fixture: &Fixture) -> Self {
        let (websocket, _) = connect_async(format!("ws://{}", fixture.addr))
            .await
            .expect("the daemon should accept a connection");
        Self {
            // From 1, the way the protocol has clients number their requests: id 0 is what a
            // message the daemon could read no id out of is answered under.
            next_id: 1,
            websocket,
            pushes: VecDeque::new(),
        }
    }

    /// Connects and presents the token, the way every connection that gets anywhere opens.
    async fn authenticated(fixture: &Fixture) -> Self {
        let mut client = Self::connect(fixture).await;
        let result: AuthResult = client
            .ok(Method::Auth(AuthParams {
                token: fixture.token.clone(),
            }))
            .await;
        assert_eq!(result.protocol_version, PROTOCOL_VERSION);
        client
    }

    /// Sends one request and reads the response to it.
    async fn call(&mut self, method: Method) -> Response {
        let id = self.next_id;
        self.next_id += 1;
        let request =
            serde_json::to_string(&Request::new(id, method)).expect("a request should serialize");
        let response = self.send(&request).await;
        assert_eq!(response.id, id, "a response answers the request it names");
        response
    }

    /// Sends a message the typed API cannot build, and reads the response to it.
    ///
    /// A watch pushes on the same connection the requests travel on, so anything that arrives
    /// before the response is set aside rather than mistaken for one.
    async fn send(&mut self, text: &str) -> Response {
        self.websocket
            .send(Message::text(text.to_owned()))
            .await
            .expect("the daemon should take the message");
        loop {
            match self.receive().await {
                Incoming::Response(response) => return response,
                Incoming::Push(push) => self.pushes.push_back(push),
            }
        }
    }

    /// The next message the daemon sends, whichever of the two kinds it is.
    async fn receive(&mut self) -> Incoming {
        let message = self
            .websocket
            .next()
            .await
            .expect("the daemon should answer")
            .expect("the answer should arrive whole");
        let Message::Text(text) = message else {
            panic!("the daemon answers in text frames, and sent {message:?}");
        };
        // A push carries `event` and no `id`, so it is the one of the two that parses.
        match serde_json::from_str(text.as_str()) {
            Ok(push) => Incoming::Push(push),
            Err(_) => Incoming::Response(
                serde_json::from_str(text.as_str())
                    .expect("the answer should be a response or a push"),
            ),
        }
    }

    /// The change a watch reports about `name`, with `apply` making that change until one comes.
    async fn change_to(
        &mut self,
        name: &str,
        apply: impl AsyncFnMut(&mut Self),
    ) -> FsChangedParams {
        self.change_to_matching(
            &format!("change to {name}"),
            |change| change.path.ends_with(name),
            apply,
        )
        .await
    }

    /// The change a watch reports that `matches`, with `apply` making it until one comes.
    ///
    /// `apply` runs once per round, so a change it cannot make twice is one to put back first.
    /// `what` names what was waited for in the failure, which a predicate cannot.
    async fn change_to_matching(
        &mut self,
        what: &str,
        matches: impl Fn(&FsChangedParams) -> bool,
        mut apply: impl AsyncFnMut(&mut Self),
    ) -> FsChangedParams {
        let deadline = Instant::now() + CHANGE_TIMEOUT;
        loop {
            apply(self).await;
            if let Some(change) = self.change_matching(CHANGE_RETRY, &matches).await {
                return change;
            }
            assert!(
                Instant::now() < deadline,
                "no {what} was reported within {CHANGE_TIMEOUT:?}"
            );
        }
    }

    /// The next change a watch reports about `name`, and `None` when the window runs out first.
    async fn change_within(&mut self, window: Duration, name: &str) -> Option<FsChangedParams> {
        self.change_matching(window, |change| change.path.ends_with(name))
            .await
    }

    /// The next change a watch reports that `matches`, and `None` when the window runs out first.
    ///
    /// Changes that do not match are passed over: a directory watch also sees the directory
    /// itself, and what a backend reports around a write is its own to decide.
    async fn change_matching(
        &mut self,
        window: Duration,
        matches: impl Fn(&FsChangedParams) -> bool,
    ) -> Option<FsChangedParams> {
        let deadline = Instant::now() + window;
        loop {
            let push = match self.pushes.pop_front() {
                Some(push) => push,
                None => match timeout_at(deadline, self.receive()).await {
                    Ok(Incoming::Push(push)) => push,
                    Ok(Incoming::Response(response)) => {
                        panic!("nothing asked for this response: {response:?}")
                    }
                    Err(_) => return None,
                },
            };
            let Event::FsChanged(change) = push.event;
            if matches(&change) {
                return Some(change);
            }
        }
    }

    /// Panics if a change about `name` arrives while the window lasts.
    async fn expect_no_change(&mut self, name: &str) {
        let change = self.change_within(NO_CHANGE_WINDOW, name).await;
        assert!(change.is_none(), "a change was reported: {change:?}");
    }

    /// The result of a call that is expected to go through, read as the method's result type.
    async fn ok<T: DeserializeOwned>(&mut self, method: Method) -> T {
        let response = self.call(method).await;
        let result = response
            .result()
            .unwrap_or_else(|| panic!("the call should have gone through: {:?}", response.error()));
        serde_json::from_value(result.clone()).expect("the result should be the method's own")
    }

    /// The error of a call that is expected to be refused.
    async fn err(&mut self, method: Method) -> ResponseError {
        let response = self.call(method).await;
        response
            .error()
            .cloned()
            .unwrap_or_else(|| panic!("the call should have been refused: {:?}", response.result()))
    }

    /// Waits for the daemon to end the connection.
    async fn expect_closed(&mut self) {
        match self.websocket.next().await {
            None | Some(Ok(Message::Close(_))) => {}
            other => panic!("the daemon should have closed the connection, and sent {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_client_reads_a_file_edits_it_with_the_core_and_writes_it_back() {
    let fixture = Fixture::start(&[("notes.txt", "alpha one\nbravo two\n")]).await;
    let mut client = Client::authenticated(&fixture).await;

    let read: FsReadResult = client
        .ok(Method::FsRead(FsReadParams {
            path: "notes.txt".to_owned(),
        }))
        .await;
    assert_eq!(read.content, "alpha one\nbravo two\n");

    let mut editor = Editor::new(&read.content);
    editor
        .handle_keys("ciwfoo<Esc>jA!<Esc>")
        .expect("the key sequence should parse");

    let _: Ack = client
        .ok(Method::FsWrite(FsWriteParams {
            path: "notes.txt".to_owned(),
            content: editor.text(),
        }))
        .await;
    assert_eq!(fixture.read("notes.txt"), "foo one\nbravo two!\n");
}

#[tokio::test]
async fn a_write_creates_a_file_that_was_not_there() {
    let fixture = Fixture::start(&[]).await;
    let mut client = Client::authenticated(&fixture).await;

    let _: Ack = client
        .ok(Method::FsWrite(FsWriteParams {
            path: "new.txt".to_owned(),
            content: "hello\n".to_owned(),
        }))
        .await;
    assert_eq!(fixture.read("new.txt"), "hello\n");
}

#[tokio::test]
async fn a_write_that_cannot_be_finished_leaves_what_was_there_and_stages_nothing() {
    // A directory is a destination the content can be staged for and never renamed over, which is
    // a write failing at the step that would have replaced the file it names.
    let fixture = Fixture::start(&[("src/main.rs", "fn main() {}\n")]).await;
    let mut client = Client::authenticated(&fixture).await;

    let error = client
        .err(Method::FsWrite(FsWriteParams {
            path: "src".to_owned(),
            content: "planted\n".to_owned(),
        }))
        .await;
    // Renaming over a directory is an `Io` matter on Unix and a permission one on Windows;
    // either way the write failed at the replacing step.
    assert!(
        matches!(error.code, ErrorCode::Io | ErrorCode::PermissionDenied),
        "{error:?}"
    );
    assert_eq!(fixture.read("src/main.rs"), "fn main() {}\n");
    assert_eq!(
        fixture.names(&fixture.root),
        ["src"],
        "a write that failed leaves nothing staged behind"
    );
}

#[tokio::test]
async fn a_write_over_the_directory_the_daemon_serves_stages_nothing_outside_it() {
    let fixture = Fixture::start(&[]).await;
    let mut client = Client::authenticated(&fixture).await;

    let error = client
        .err(Method::FsWrite(FsWriteParams {
            path: ".".to_owned(),
            content: "planted\n".to_owned(),
        }))
        .await;
    assert_eq!(error.code, ErrorCode::Io);
    assert_eq!(
        fixture.names(fixture.directory.path()),
        ["root", "secret.md"],
        "nothing is written next to the directory this daemon serves"
    );
}

#[tokio::test]
async fn two_writes_of_one_path_at_the_same_time_leave_one_of_them_whole() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut one = Client::authenticated(&fixture).await;
    let mut other = Client::authenticated(&fixture).await;
    // Lengths far apart, so that a file made of one write's opening and the other's tail is a file
    // that matches neither.
    let short = "short\n".to_owned();
    let long = "a line long enough to be told from the other one\n".repeat(256);

    let (_, _): (Ack, Ack) = tokio::join!(
        one.ok(Method::FsWrite(FsWriteParams {
            path: "notes.txt".to_owned(),
            content: short.clone(),
        })),
        other.ok(Method::FsWrite(FsWriteParams {
            path: "notes.txt".to_owned(),
            content: long.clone(),
        })),
    );

    let written = fixture.read("notes.txt");
    assert!(
        written == short || written == long,
        "one of the two writes is what is left, and {} bytes were",
        written.len()
    );
}

#[tokio::test]
async fn a_listing_names_what_is_directly_under_the_directory() {
    let fixture =
        Fixture::start(&[("notes.txt", "hello\n"), ("src/main.rs", "fn main() {}\n")]).await;
    let mut client = Client::authenticated(&fixture).await;

    let mut listing: FsListResult = client
        .ok(Method::FsList(FsListParams {
            path: ".".to_owned(),
        }))
        .await;
    // The daemon lists in the order it read the directory, which is the file system's to choose.
    listing
        .entries
        .sort_by(|one, other| one.name.cmp(&other.name));
    assert_eq!(
        listing.entries,
        [
            DirEntry {
                name: "notes.txt".to_owned(),
                kind: EntryKind::File,
            },
            DirEntry {
                name: "src".to_owned(),
                kind: EntryKind::Directory,
            },
        ]
    );
}

/// A FIFO under the listed directory is neither a file nor a directory nor a symlink, and is
/// reported as [`EntryKind::Other`] rather than misclassified as [`EntryKind::File`].
#[cfg(unix)]
#[tokio::test]
async fn a_fifo_is_listed_as_other_rather_than_as_a_file() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let status = std::process::Command::new("mkfifo")
        .arg(fixture.root.join("events"))
        .status()
        .expect("mkfifo should run");
    assert!(status.success(), "mkfifo should create the fifo");
    let mut client = Client::authenticated(&fixture).await;

    let mut listing: FsListResult = client
        .ok(Method::FsList(FsListParams {
            path: ".".to_owned(),
        }))
        .await;
    listing
        .entries
        .sort_by(|one, other| one.name.cmp(&other.name));
    assert_eq!(
        listing.entries,
        [
            DirEntry {
                name: "events".to_owned(),
                kind: EntryKind::Other,
            },
            DirEntry {
                name: "notes.txt".to_owned(),
                kind: EntryKind::File,
            },
        ]
    );
}

/// The path a client composes for a child of a listing — the listed directory's path, `/`, and
/// the entry's name — is one `fs.read` takes back, which is the contract `fs.list`'s doc comments
/// make (`crates/wim-protocol/src/fs.rs`).
#[tokio::test]
async fn a_child_path_composed_from_a_listing_reads_back_the_child() {
    let fixture = Fixture::start(&[("src/nested/deep.txt", "found\n")]).await;
    let mut client = Client::authenticated(&fixture).await;

    let top: FsListResult = client
        .ok(Method::FsList(FsListParams {
            path: ".".to_owned(),
        }))
        .await;
    let src = &top.entries[0];
    assert_eq!(src.name, "src");
    let src_path = src.name.clone();

    let middle: FsListResult = client
        .ok(Method::FsList(FsListParams {
            path: src_path.clone(),
        }))
        .await;
    let nested = &middle.entries[0];
    assert_eq!(nested.name, "nested");
    let nested_path = format!("{src_path}/{}", nested.name);

    let bottom: FsListResult = client
        .ok(Method::FsList(FsListParams {
            path: nested_path.clone(),
        }))
        .await;
    let deep = &bottom.entries[0];
    assert_eq!(deep.name, "deep.txt");
    let deep_path = format!("{nested_path}/{}", deep.name);

    let read: FsReadResult = client
        .ok(Method::FsRead(FsReadParams { path: deep_path }))
        .await;
    assert_eq!(read.content, "found\n");
}

#[tokio::test]
async fn a_connection_that_presents_the_wrong_token_is_refused_and_dropped() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut client = Client::connect(&fixture).await;

    let error = client
        .err(Method::Auth(AuthParams {
            token: "not-the-token".to_owned(),
        }))
        .await;
    assert_eq!(error.code, ErrorCode::Unauthorized);
    client.expect_closed().await;
}

#[tokio::test]
async fn a_request_that_comes_before_auth_is_refused_and_the_connection_dropped() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut client = Client::connect(&fixture).await;

    let error = client
        .err(Method::FsRead(FsReadParams {
            path: "notes.txt".to_owned(),
        }))
        .await;
    assert_eq!(error.code, ErrorCode::Unauthorized);
    client.expect_closed().await;
}

#[tokio::test]
async fn a_connection_that_never_finishes_the_handshake_is_let_go() {
    let fixture = Fixture::start(&[]).await;
    let mut socket = TcpStream::connect(fixture.addr)
        .await
        .expect("the daemon should take the connection");

    let mut byte = [0u8; 1];
    let read = timeout(UNAUTHENTICATED_WINDOW, socket.read(&mut byte))
        .await
        .expect("the daemon should not hold a connection that says nothing");
    // The daemon drops the socket, which reaches this side as the end of the stream, or as the
    // connection being reset when the two cross.
    assert!(
        matches!(read, Ok(0) | Err(_)),
        "the connection should be over, and reading it gave {read:?}"
    );
}

#[tokio::test]
async fn a_connection_that_never_presents_the_token_is_let_go() {
    let fixture = Fixture::start(&[]).await;
    let mut client = Client::connect(&fixture).await;

    timeout(UNAUTHENTICATED_WINDOW, client.expect_closed())
        .await
        .expect("the daemon should not hold a connection that never authenticates");
}

#[tokio::test]
async fn a_connection_that_goes_away_before_it_is_served_leaves_the_daemon_serving() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    // A socket closed the moment it is connected is what leaves an aborted connection in the
    // listen queue, which is one of the ways `accept` fails over a listener that is still good.
    for _ in 0..8 {
        drop(
            TcpStream::connect(fixture.addr)
                .await
                .expect("the daemon should take the connection"),
        );
    }

    let mut client = Client::authenticated(&fixture).await;
    let read: FsReadResult = client
        .ok(Method::FsRead(FsReadParams {
            path: "notes.txt".to_owned(),
        }))
        .await;
    assert_eq!(read.content, "hello\n");
}

#[tokio::test]
async fn a_path_that_reaches_outside_the_root_is_refused() {
    let fixture = Fixture::start(&[("src/main.rs", "fn main() {}\n")]).await;
    let outside = fixture.outside("secret.md").display().to_string();
    let mut client = Client::authenticated(&fixture).await;

    for path in [
        "../secret.md",
        "./../secret.md",
        "src/../../secret.md",
        &outside,
    ] {
        let error = client
            .err(Method::FsRead(FsReadParams {
                path: path.to_owned(),
            }))
            .await;
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{path}");
    }

    let error = client
        .err(Method::FsWrite(FsWriteParams {
            path: "../planted.md".to_owned(),
            content: "planted\n".to_owned(),
        }))
        .await;
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(
        !fixture.outside("planted.md").exists(),
        "a refused write leaves nothing behind"
    );
}

#[tokio::test]
async fn a_path_inside_the_root_is_served_however_it_is_written() {
    let fixture = Fixture::start(&[("src/main.rs", "fn main() {}\n")]).await;
    let inside = fixture.root.join("src").join("main.rs");
    let mut client = Client::authenticated(&fixture).await;

    for path in [
        "src/main.rs".to_owned(),
        "./src/main.rs".to_owned(),
        "src/../src/main.rs".to_owned(),
        inside.display().to_string(),
    ] {
        let read: FsReadResult = client
            .ok(Method::FsRead(FsReadParams { path: path.clone() }))
            .await;
        assert_eq!(read.content, "fn main() {}\n", "{path}");
    }
}

/// A link under the root is followed for as long as it stays under it, and refused where it leads
/// out with the answer a path that leaves the root lexically gets. The refusal is now the open's
/// rather than a check taken before it, and this is what fixes that the client cannot tell
/// (`documents/adr/0003-daemon-beneath-semantics-with-cap-std.md`).
#[cfg(unix)]
#[tokio::test]
async fn a_link_is_read_through_inside_the_root_and_refused_where_it_leads_out_of_it() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    // Relative, because what an absolute link target names is a path in the file system the daemon
    // was started in rather than one under the root.
    std::os::unix::fs::symlink("notes.txt", fixture.root.join("inside.txt"))
        .expect("the link should be created");
    std::os::unix::fs::symlink(
        fixture.outside("secret.md"),
        fixture.root.join("outside.txt"),
    )
    .expect("the link should be created");
    let mut client = Client::authenticated(&fixture).await;

    let read: FsReadResult = client
        .ok(Method::FsRead(FsReadParams {
            path: "inside.txt".to_owned(),
        }))
        .await;
    assert_eq!(read.content, "hello\n");

    let error = client
        .err(Method::FsRead(FsReadParams {
            path: "outside.txt".to_owned(),
        }))
        .await;
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(
        error.message,
        "outside.txt: outside the directory this daemon serves"
    );
}

#[tokio::test]
async fn a_path_that_is_not_there_is_reported_as_missing() {
    let fixture = Fixture::start(&[]).await;
    let mut client = Client::authenticated(&fixture).await;

    let error = client
        .err(Method::FsRead(FsReadParams {
            path: "nowhere.txt".to_owned(),
        }))
        .await;
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn a_watch_reports_a_write_the_client_made_through_the_daemon() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut client = Client::authenticated(&fixture).await;

    let watch: FsWatchResult = client
        .ok(Method::FsWatch(FsWatchParams {
            path: "notes.txt".to_owned(),
            recursive: false,
        }))
        .await;

    let mut change = client
        .change_to("notes.txt", async |client: &mut Client| {
            let _: Ack = client
                .ok(Method::FsWrite(FsWriteParams {
                    path: "notes.txt".to_owned(),
                    content: "written\n".to_owned(),
                }))
                .await;
        })
        .await;
    if change.kind == FsChangeKind::Removed {
        // The rename that replaces the file reaches some backends as its two halves, the
        // replaced file going first; the half that says the file is there again follows it.
        change = client
            .change_within(CHANGE_RETRY, "notes.txt")
            .await
            .expect("the arriving half of the replacing rename should follow");
    }

    assert_eq!(change.watch_id, watch.watch_id);
    // A write over a file that was made moments ago reaches the backends as one event or two, and
    // whether they call it a creation or a change of contents is theirs to decide.
    assert!(
        matches!(change.kind, FsChangeKind::Created | FsChangeKind::Modified),
        "{change:?}"
    );
}

#[tokio::test]
async fn a_watch_reports_a_change_made_outside_the_daemon() {
    let fixture = Fixture::start(&[]).await;
    let mut client = Client::authenticated(&fixture).await;

    let watch: FsWatchResult = client
        .ok(Method::FsWatch(FsWatchParams {
            path: ".".to_owned(),
            recursive: true,
        }))
        .await;

    let change = client
        .change_to("planted.txt", async |_: &mut Client| {
            fixture.write("planted.txt", "planted\n")
        })
        .await;

    assert_eq!(change.watch_id, watch.watch_id);
    assert!(
        change.path.ends_with("planted.txt"),
        "the change names the path that changed: {change:?}"
    );
}

#[tokio::test]
async fn a_watch_that_was_dropped_reports_nothing_more() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut client = Client::authenticated(&fixture).await;

    let watch: FsWatchResult = client
        .ok(Method::FsWatch(FsWatchParams {
            path: ".".to_owned(),
            recursive: false,
        }))
        .await;
    // The watch is live before it is dropped, so that what follows says something.
    let change = client
        .change_to("notes.txt", async |_: &mut Client| {
            fixture.write("notes.txt", "watched\n")
        })
        .await;
    assert_eq!(change.watch_id, watch.watch_id);

    let _: Ack = client
        .ok(Method::FsUnwatch(FsUnwatchParams {
            watch_id: watch.watch_id,
        }))
        .await;

    // A file of its own, so that a change the watch saw before it was dropped is not mistaken for
    // one it saw after.
    fixture.write("after-unwatch.txt", "unwatched\n");
    client.expect_no_change("after-unwatch.txt").await;
}

#[tokio::test]
async fn dropping_a_watch_that_is_not_there_is_the_state_it_asks_for() {
    let fixture = Fixture::start(&[]).await;
    let mut client = Client::authenticated(&fixture).await;

    let watch: FsWatchResult = client
        .ok(Method::FsWatch(FsWatchParams {
            path: ".".to_owned(),
            recursive: false,
        }))
        .await;
    for _ in 0..2 {
        let _: Ack = client
            .ok(Method::FsUnwatch(FsUnwatchParams {
                watch_id: watch.watch_id,
            }))
            .await;
    }
    let _: Ack = client
        .ok(Method::FsUnwatch(FsUnwatchParams { watch_id: 404 }))
        .await;
}

#[tokio::test]
async fn a_change_is_reported_only_to_the_connection_that_asked_to_see_it() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut watcher = Client::authenticated(&fixture).await;
    let mut writer = Client::authenticated(&fixture).await;

    let watch: FsWatchResult = watcher
        .ok(Method::FsWatch(FsWatchParams {
            path: ".".to_owned(),
            recursive: false,
        }))
        .await;

    let change = watcher
        .change_to("notes.txt", async |_: &mut Client| {
            let _: Ack = writer
                .ok(Method::FsWrite(FsWriteParams {
                    path: "notes.txt".to_owned(),
                    content: "written by the other one\n".to_owned(),
                }))
                .await;
        })
        .await;

    assert_eq!(change.watch_id, watch.watch_id);
    writer.expect_no_change("notes.txt").await;
}

#[tokio::test]
async fn a_watch_on_a_path_outside_the_root_is_refused() {
    let fixture = Fixture::start(&[]).await;
    let outside = fixture.outside("secret.md").display().to_string();
    let mut client = Client::authenticated(&fixture).await;

    for path in ["..".to_owned(), "../secret.md".to_owned(), outside] {
        let error = client
            .err(Method::FsWatch(FsWatchParams {
                path: path.clone(),
                recursive: true,
            }))
            .await;
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{path}");
    }
}

#[tokio::test]
async fn a_watch_on_a_path_that_is_not_there_is_reported_as_missing() {
    let fixture = Fixture::start(&[]).await;
    let mut client = Client::authenticated(&fixture).await;

    let error = client
        .err(Method::FsWatch(FsWatchParams {
            path: "nowhere.txt".to_owned(),
            recursive: false,
        }))
        .await;
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn a_name_the_daemon_keeps_for_itself_is_neither_listed_nor_served() {
    // A file at a staging name, left where one would be by a write that was interrupted or by
    // something else on the machine; either way it is not a client's to see or to touch.
    let fixture = Fixture::start(&[
        ("notes.txt", "hello\n"),
        (".wim-0123456789abcdef", "staged\n"),
    ])
    .await;
    let mut client = Client::authenticated(&fixture).await;

    let listing: FsListResult = client
        .ok(Method::FsList(FsListParams {
            path: ".".to_owned(),
        }))
        .await;
    assert_eq!(
        listing.entries,
        [DirEntry {
            name: "notes.txt".to_owned(),
            kind: EntryKind::File,
        }],
        "what this daemon stages under is not part of the directory it serves"
    );

    for method in [
        Method::FsRead(FsReadParams {
            path: ".wim-0123456789abcdef".to_owned(),
        }),
        Method::FsWrite(FsWriteParams {
            path: "./.wim-0123456789abcdef".to_owned(),
            content: "planted\n".to_owned(),
        }),
        Method::FsWatch(FsWatchParams {
            path: ".wim-0123456789abcdef".to_owned(),
            recursive: false,
        }),
        Method::FsList(FsListParams {
            path: ".wim-0123456789abcdef".to_owned(),
        }),
    ] {
        let error = client.err(method).await;
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{error:?}");
    }
    assert_eq!(
        fixture.read(".wim-0123456789abcdef"),
        "staged\n",
        "a refused write leaves what was there"
    );
}

#[tokio::test]
async fn a_watch_reports_nothing_about_the_file_a_write_stages() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut client = Client::authenticated(&fixture).await;

    let _: FsWatchResult = client
        .ok(Method::FsWatch(FsWatchParams {
            path: ".".to_owned(),
            recursive: true,
        }))
        .await;
    // The write stages its content beside the destination and renames it over: both steps happen
    // in the directory being watched.
    let _: Ack = client
        .ok(Method::FsWrite(FsWriteParams {
            path: "notes.txt".to_owned(),
            content: "written\n".to_owned(),
        }))
        .await;

    let staged = client
        .change_matching(NO_CHANGE_WINDOW, |change| {
            Path::new(&change.path)
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".wim-"))
        })
        .await;
    assert!(staged.is_none(), "a change was reported: {staged:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_watch_on_a_link_reports_the_link_itself_being_removed() {
    // The link points at a file in the root, so that what the two ways of resolving it would watch
    // both exist: the link's own name, and the file it points at.
    let fixture = Fixture::start(&[("target.txt", "hello\n")]).await;
    let link = fixture.root.join("link.txt");
    std::os::unix::fs::symlink(fixture.root.join("target.txt"), &link)
        .expect("the link should be created");
    let mut client = Client::authenticated(&fixture).await;

    let watch: FsWatchResult = client
        .ok(Method::FsWatch(FsWatchParams {
            path: "link.txt".to_owned(),
            recursive: false,
        }))
        .await;

    let target = fixture.root.join("target.txt");
    let change = client
        .change_to_matching(
            "removal of link.txt",
            |change| change.path.ends_with("link.txt") && change.kind == FsChangeKind::Removed,
            async |_: &mut Client| {
                // Put back before it is removed again, so that every round is the same removal.
                if std::fs::symlink_metadata(&link).is_err() {
                    std::os::unix::fs::symlink(&target, &link).expect("the link should be created");
                }
                std::fs::remove_file(&link).expect("the link should be removed");
            },
        )
        .await;

    assert_eq!(change.watch_id, watch.watch_id);
    assert_eq!(
        fixture.read("target.txt"),
        "hello\n",
        "the file the link pointed at is untouched"
    );
}

#[tokio::test]
async fn a_connection_holds_only_so_many_watches() {
    let fixture = Fixture::start(&[("locked/notes.txt", "hello\n")]).await;
    // A directory that cannot be written in is one a watch is answered for without waiting to see
    // a probe, which is both a case of its own and what keeps a test that takes every watch a
    // connection may hold from waiting out one readiness window per watch.
    let locked = fixture.root.join("locked");
    let mut permissions = std::fs::metadata(&locked)
        .expect("the directory should be there")
        .permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&locked, permissions).expect("the permissions should be set");
    let mut client = Client::authenticated(&fixture).await;

    let mut watch_ids = Vec::new();
    loop {
        let response = client
            .call(Method::FsWatch(FsWatchParams {
                path: "locked".to_owned(),
                recursive: false,
            }))
            .await;
        match response.result() {
            Some(result) => {
                let watch: FsWatchResult =
                    serde_json::from_value(result.clone()).expect("the result should be a watch");
                watch_ids.push(watch.watch_id);
                assert!(
                    watch_ids.len() <= 1024,
                    "a connection took more watches than a bound would allow"
                );
            }
            None => {
                let error = response.error().expect("a refusal carries an error");
                assert_eq!(error.code, ErrorCode::InvalidRequest, "{error:?}");
                assert!(
                    error.message.contains("fs.unwatch"),
                    "the refusal says how to make room: {}",
                    error.message
                );
                break;
            }
        }
    }

    // Dropping one is what makes room for the next, which is what the refusal told the client.
    let _: Ack = client
        .ok(Method::FsUnwatch(FsUnwatchParams {
            watch_id: watch_ids[0],
        }))
        .await;
    let _: FsWatchResult = client
        .ok(Method::FsWatch(FsWatchParams {
            path: "locked".to_owned(),
            recursive: false,
        }))
        .await;
}

#[tokio::test]
async fn a_message_of_another_protocol_version_is_refused_under_the_id_it_carried() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut client = Client::authenticated(&fixture).await;

    let response = client
        .send(r#"{"v":2,"id":41,"method":"fs.read","params":{"path":"notes.txt"}}"#)
        .await;
    assert_eq!(response.id, 41);
    assert_eq!(
        response.error().map(|error| &error.code),
        Some(&ErrorCode::UnsupportedVersion)
    );
}

#[tokio::test]
async fn a_message_that_is_not_a_request_is_refused_and_the_connection_lives_on() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut client = Client::authenticated(&fixture).await;

    for (text, id) in [
        ("{ not json", 0),
        (r#"{"v":1,"id":7,"method":"fs.explode","params":{}}"#, 7),
        (r#"{"v":1,"id":8,"method":"fs.read","params":{}}"#, 8),
    ] {
        let response = client.send(text).await;
        assert_eq!(response.id, id, "{text}");
        assert_eq!(
            response.error().map(|error| &error.code),
            Some(&ErrorCode::InvalidRequest),
            "{text}"
        );
    }

    // Nothing about a message the daemon could not read ends the connection: the client that sent
    // it goes on working.
    let read: FsReadResult = client
        .ok(Method::FsRead(FsReadParams {
            path: "notes.txt".to_owned(),
        }))
        .await;
    assert_eq!(read.content, "hello\n");
}

#[tokio::test]
async fn two_clients_are_served_at_the_same_time() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut one = Client::authenticated(&fixture).await;
    let mut other = Client::authenticated(&fixture).await;

    let _: Ack = one
        .ok(Method::FsWrite(FsWriteParams {
            path: "notes.txt".to_owned(),
            content: "written by one\n".to_owned(),
        }))
        .await;
    // Phase 2 is last-write-wins, so what the other client reads is what was written last.
    let read: FsReadResult = other
        .ok(Method::FsRead(FsReadParams {
            path: "notes.txt".to_owned(),
        }))
        .await;
    assert_eq!(read.content, "written by one\n");
}

#[tokio::test]
async fn a_daemon_reports_the_directory_it_serves_and_the_port_it_took() {
    let fixture = Fixture::start(&[]).await;
    let daemon = Daemon::bind(loopback(), &fixture.root)
        .await
        .expect("the daemon should bind");

    assert_eq!(
        daemon.root(),
        std::fs::canonicalize(&fixture.root)
            .expect("the root should resolve")
            .as_path()
    );
    let addr = daemon
        .local_addr()
        .expect("the daemon should have taken one");
    assert!(addr.ip().is_loopback());
    assert_ne!(
        addr.port(),
        0,
        "the port asked for is picked before serving"
    );
    assert_ne!(
        daemon.token(),
        fixture.token,
        "each daemon has its own token"
    );
}
