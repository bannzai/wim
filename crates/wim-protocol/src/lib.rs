//! Wire protocol shared by the wim daemon and its clients.
//!
//! The daemon is a file system provider: it lists, reads, writes and watches files, and the
//! editing buffer lives in the client (`documents/adr/0001-daemon-fs-provider.md`). This crate
//! holds the messages that cross that boundary, and nothing else — no IO and no async runtime,
//! so that it keeps compiling for `wasm32-unknown-unknown` as well as native targets.
//!
//! Messages travel as JSON in WebSocket text frames. Every one of them carries the protocol
//! version as `v`, so the JSON these types produce is the crate's public contract and is pinned
//! by tests.

pub mod error;
pub mod fs;
pub mod message;

pub use error::{ErrorCode, ResponseError};
pub use fs::{
    Ack, AuthParams, AuthResult, DirEntry, EntryKind, FsChangeKind, FsChangedParams, FsListParams,
    FsListResult, FsReadParams, FsReadResult, FsUnwatchParams, FsWatchParams, FsWatchResult,
    FsWriteParams,
};
pub use message::{
    Event, Method, PROTOCOL_VERSION, Request, Response, ResponsePayload, ServerPush,
    is_supported_version,
};
