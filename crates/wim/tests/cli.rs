//! End-to-end tests of the `wim` binary.
//!
//! What the binary adds over the daemon library is the way a client learns how to reach it, so
//! these tests start `wim serve`, read the address and the token off its standard output, and go
//! on to open, edit and save a file with them.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use wim_protocol::{
    Ack, AuthParams, AuthResult, FsReadParams, FsReadResult, FsWriteParams, Method, Request,
    Response,
};

/// A running `wim serve`, stopped when the test that started it ends, panic or not.
struct Serve {
    child: Child,
    directory: TempDir,
    addr: String,
    token: String,
}

impl Serve {
    /// Starts the binary over a directory holding `files`, given as `(name, content)` pairs, and
    /// reads what it printed about how to reach it.
    fn start(files: &[(&str, &str)]) -> Self {
        let directory = TempDir::new().expect("a temporary directory should be available");
        for (name, content) in files {
            std::fs::write(directory.path().join(name), content)
                .expect("the file should be written");
        }
        let mut child = Command::cargo_bin("wim")
            .expect("the binary should be built")
            .args(["serve", "--root"])
            .arg(directory.path())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the binary should start");
        let stdout = child
            .stdout
            .take()
            .expect("standard output should be piped");
        let mut lines = BufReader::new(stdout).lines();
        let mut next = |prefix: &str| {
            let line = lines
                .next()
                .expect("the daemon should report how it can be reached")
                .expect("the line should be readable");
            line.strip_prefix(prefix)
                .unwrap_or_else(|| panic!("the line {line:?} should open with {prefix:?}"))
                .to_owned()
        };
        let addr = next("listening on ");
        let token = next("token: ");
        // The root line is read too, so that the pipe is not closed while the daemon is still
        // in the middle of writing it.
        next("root: ");
        Self {
            child,
            directory,
            addr,
            token,
        }
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.directory.path().join(name))
            .expect("the file should be readable")
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        // The daemon serves until it is stopped, so the test has to be the one to stop it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The client side of one connection.
type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Sends one request and reads back the result of a call that is expected to go through.
async fn call<T: DeserializeOwned>(client: &mut Client, id: u64, method: Method) -> T {
    let request =
        serde_json::to_string(&Request::new(id, method)).expect("a request should serialize");
    client
        .send(Message::text(request))
        .await
        .expect("the daemon should take the request");
    let message = client
        .next()
        .await
        .expect("the daemon should answer")
        .expect("the answer should arrive whole");
    let Message::Text(text) = message else {
        panic!("the daemon answers in text frames, and sent {message:?}");
    };
    let response: Response =
        serde_json::from_str(text.as_str()).expect("the answer should be a response");
    assert_eq!(response.id, id, "a response answers the request it names");
    let result = response
        .result()
        .unwrap_or_else(|| panic!("the call should have gone through: {:?}", response.error()));
    serde_json::from_value(result.clone()).expect("the result should be the method's own")
}

#[tokio::test]
async fn a_client_opens_and_saves_a_file_with_the_address_and_token_the_binary_printed() {
    let serve = Serve::start(&[("notes.txt", "hello\n")]);
    let (mut client, _) = connect_async(format!("ws://{}", serve.addr))
        .await
        .expect("the daemon should accept a connection");

    // Numbered from 1, the way the protocol has clients number their requests: id 0 is what a
    // message the daemon could read no id out of is answered under.
    let _: AuthResult = call(
        &mut client,
        1,
        Method::Auth(AuthParams {
            token: serve.token.clone(),
        }),
    )
    .await;
    let read: FsReadResult = call(
        &mut client,
        2,
        Method::FsRead(FsReadParams {
            path: "notes.txt".to_owned(),
        }),
    )
    .await;
    assert_eq!(read.content, "hello\n");

    let _: Ack = call(
        &mut client,
        3,
        Method::FsWrite(FsWriteParams {
            path: "notes.txt".to_owned(),
            content: "hello again\n".to_owned(),
        }),
    )
    .await;
    assert_eq!(serve.read("notes.txt"), "hello again\n");
}

#[test]
fn serve_reports_the_loopback_address_it_took_and_a_token_to_present() {
    let serve = Serve::start(&[]);
    assert!(
        serve.addr.starts_with("127.0.0.1:"),
        "the daemon listens on loopback, and reported {:?}",
        serve.addr
    );
    assert_ne!(
        serve.addr, "127.0.0.1:0",
        "the port the operating system picked is the one reported"
    );
    assert!(!serve.token.is_empty());
}
