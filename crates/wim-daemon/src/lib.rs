//! The wim daemon: a file system provider that clients reach over WebSocket.
//!
//! It lists, reads, writes and watches files under one root directory and does nothing else. A
//! watch reports what changed to the connection that asked for it, and lives as long as that
//! connection does. The editing
//! buffer lives in the client, which is why this crate does not depend on `wim-core`
//! (`documents/adr/0001-daemon-fs-provider.md`). The messages it speaks are `wim-protocol`'s,
//! carried as JSON in text frames.
//!
//! A connection has to open with an `auth` message presenting the token [`Daemon::token`]
//! returns; anything else is answered with an error and the connection is dropped.
//!
//! ```no_run
//! # async fn serve() -> std::io::Result<()> {
//! let daemon = wim_daemon::Daemon::bind("127.0.0.1:0".parse().unwrap(), ".").await?;
//! println!("listening on {}", daemon.local_addr()?);
//! println!("token: {}", daemon.token());
//! daemon.serve().await
//! # }
//! ```

mod root;
mod session;
mod watch;

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use tokio::net::TcpListener;
use wim_protocol::{ErrorCode, ResponseError};

pub use root::Root;

/// How many random bytes a token is made of. 128 bits, so that a token cannot be guessed by a
/// process that is already on the machine, which is what the token defends against
/// (`documents/adr/0001-daemon-fs-provider.md`).
const TOKEN_BYTES: usize = 16;

/// A daemon that has taken its port and is ready to serve.
///
/// Binding and serving are two steps so that the caller can report the address and the token —
/// which the client needs before it can connect — while the daemon is already listening.
#[derive(Debug)]
pub struct Daemon {
    listener: TcpListener,
    shared: Arc<Shared>,
}

/// What every connection of one daemon reads.
#[derive(Debug)]
struct Shared {
    root: Root,
    token: String,
}

impl Daemon {
    /// Takes `addr` and anchors the daemon at `root`.
    ///
    /// The root is resolved once, here, so that a path a request names is compared against a
    /// directory that holds no symlink of its own.
    pub async fn bind(addr: SocketAddr, root: impl AsRef<Path>) -> io::Result<Self> {
        let root = Root::new(root).await?;
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            listener,
            shared: Arc::new(Shared {
                root,
                token: new_token(),
            }),
        })
    }

    /// The address the daemon is listening on, which is the port the operating system picked
    /// when the caller asked for port 0.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// The directory the daemon serves, as it resolved.
    pub fn root(&self) -> &Path {
        self.shared.root.path()
    }

    /// The token a client has to present in its `auth` message.
    pub fn token(&self) -> &str {
        &self.shared.token
    }

    /// Serves connections until accepting one fails.
    ///
    /// Each connection is served by a task of its own, and a connection that goes wrong — a
    /// handshake that fails, a client that disappears mid-message — takes only itself down.
    pub async fn serve(self) -> io::Result<()> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            let shared = Arc::clone(&self.shared);
            tokio::spawn(async move {
                let _ = session::serve(stream, shared).await;
            });
        }
    }
}

/// A fresh token, as hex.
///
/// Not idempotent, and has to be: the point of the token is that each daemon presents one that
/// nothing else has seen.
fn new_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).expect("the operating system should have a random generator");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A file system error, as the response the client reading it can match on.
///
/// `path` is what the request asked for rather than what it resolved to, so that the message
/// names the path the client knows.
fn io_error(path: &str, error: io::Error) -> ResponseError {
    let code = match error.kind() {
        io::ErrorKind::NotFound => ErrorCode::NotFound,
        io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        _ => ErrorCode::Io,
    };
    ResponseError::new(code, format!("{path}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_hex_and_covers_every_byte_it_was_made_of() {
        let token = new_token();
        assert_eq!(token.len(), TOKEN_BYTES * 2);
        assert!(token.chars().all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn two_tokens_do_not_match() {
        assert_ne!(new_token(), new_token());
    }

    #[test]
    fn a_missing_path_is_reported_as_not_found_under_the_name_the_request_used() {
        let error = io_error(
            "notes.md",
            io::Error::new(io::ErrorKind::NotFound, "no such file or directory"),
        );
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(error.message.starts_with("notes.md: "), "{}", error.message);
    }
}
