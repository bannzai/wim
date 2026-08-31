//! End-to-end tests over a running daemon.
//!
//! The acceptance test of the daemon is the round trip a real client makes: read a file over the
//! WebSocket, edit it with `wim-core` on this side of the wire — where the buffer lives
//! (`documents/adr/0001-daemon-fs-provider.md`) — write it back, and find the result on disk.

use std::net::SocketAddr;
use std::path::PathBuf;

use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use wim_core::Editor;
use wim_daemon::Daemon;
use wim_protocol::{
    Ack, AuthParams, AuthResult, DirEntry, EntryKind, ErrorCode, FsListParams, FsListResult,
    FsReadParams, FsReadResult, FsUnwatchParams, FsWatchParams, FsWriteParams, Method,
    PROTOCOL_VERSION, Request, Response, ResponseError,
};

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
}

/// Loopback and a port the operating system picks, so that tests can run side by side.
fn loopback() -> SocketAddr {
    "127.0.0.1:0"
        .parse()
        .expect("the address should be an address")
}

/// A client of the daemon, playing the part a wim front end plays.
struct Client {
    websocket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Client {
    /// Connects without saying anything yet.
    async fn connect(fixture: &Fixture) -> Self {
        let (websocket, _) = connect_async(format!("ws://{}", fixture.addr))
            .await
            .expect("the daemon should accept a connection");
        Self {
            websocket,
            next_id: 0,
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
    async fn send(&mut self, text: &str) -> Response {
        self.websocket
            .send(Message::text(text.to_owned()))
            .await
            .expect("the daemon should take the message");
        let message = self
            .websocket
            .next()
            .await
            .expect("the daemon should answer")
            .expect("the answer should arrive whole");
        let Message::Text(text) = message else {
            panic!("the daemon answers in text frames, and sent {message:?}");
        };
        serde_json::from_str(text.as_str()).expect("the answer should be a response")
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
async fn watching_is_refused_as_a_method_this_daemon_does_not_serve_yet() {
    let fixture = Fixture::start(&[("notes.txt", "hello\n")]).await;
    let mut client = Client::authenticated(&fixture).await;

    for method in [
        Method::FsWatch(FsWatchParams {
            path: ".".to_owned(),
            recursive: true,
        }),
        Method::FsUnwatch(FsUnwatchParams { watch_id: 1 }),
    ] {
        let error = client.err(method).await;
        assert_eq!(error.code, ErrorCode::Other("not_implemented".to_owned()));
    }
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
