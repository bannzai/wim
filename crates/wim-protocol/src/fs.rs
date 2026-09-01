//! Parameters and results of the methods the daemon serves.
//!
//! Paths cross the wire with `/` between their components whatever the daemon's own platform
//! writes its paths with, so that a client composes one the same way wherever the daemon runs
//! ([`FsListParams::path`]); the crate itself neither parses nor normalizes them.

use serde::{Deserialize, Serialize};

/// `auth` params. The token the daemon printed on startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthParams {
    /// The token, presented in the first message of a connection.
    pub token: String,
}

/// `auth` result. Lets a client see what the other side speaks before it sends anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthResult {
    /// The protocol version the daemon was built against.
    pub protocol_version: u32,
}

/// Result of a method that has nothing to report beyond having run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {}

/// `fs.list` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListParams {
    /// Directory to list. Not recursive.
    ///
    /// Read from the directory the daemon serves, which is named `.` or `""`, with `/` between the
    /// components below it — on Windows too, where the daemon takes `/` as well as the `\` its
    /// own platform writes. That is what lets a client hold a path without knowing which platform
    /// the daemon runs on, and what [`DirEntry::name`] is composed onto.
    pub path: String,
}

/// `fs.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListResult {
    /// One entry per direct child, in the order the daemon read them.
    pub entries: Vec<DirEntry>,
}

/// A single child of a listed directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    /// File name, not the full path.
    ///
    /// The path of this child is the listed directory's own path, a `/`, and this name — or this
    /// name alone when the directory listed was `.` or `""`. That path is what the client passes
    /// back to `fs.read`, `fs.write` and `fs.watch`, and it is composed the same way whichever
    /// platform the daemon runs on ([`FsListParams::path`]).
    pub name: String,
    /// What the name points at.
    pub kind: EntryKind,
}

/// What a [`DirEntry`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory, listable with another `fs.list`.
    Directory,
    /// A symlink, reported without following it.
    Symlink,
    /// Anything else a directory may hold: a socket, a FIFO, a device.
    ///
    /// Named rather than reported as a file, because what a client does with a file is open it and
    /// read it whole, and a FIFO answers that with a read that never returns.
    ///
    /// Added without raising [`crate::PROTOCOL_VERSION`], which a value old clients cannot parse
    /// would otherwise call for: before 1.0 the clients of this protocol live in this repository
    /// and are released with the daemon, so there is no build in the wild to meet `other`.
    Other,
}

/// `fs.read` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadParams {
    /// File to read.
    pub path: String,
}

/// `fs.read` result.
///
/// Only text is carried: the editing core works on text, so a file that is not valid UTF-8 comes
/// back as an error rather than as bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadResult {
    /// The whole file.
    pub content: String,
}

/// `fs.write` params.
///
/// The write is last-write-wins: the daemon does not check whether the file changed since the
/// client read it (`documents/adr/0001-daemon-fs-provider.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWriteParams {
    /// File to write, created when it does not exist.
    pub path: String,
    /// The whole file, replacing what is there.
    pub content: String,
}

/// `fs.watch` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWatchParams {
    /// File or directory to watch.
    pub path: String,
    /// Whether a watched directory also reports changes below it.
    pub recursive: bool,
}

/// `fs.watch` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWatchResult {
    /// Names this watch in the `fs.changed` events it produces and in `fs.unwatch`.
    pub watch_id: u64,
}

/// `fs.unwatch` params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsUnwatchParams {
    /// The watch to drop, as [`FsWatchResult`] named it.
    pub watch_id: u64,
}

/// `fs.changed` params: one change under a watch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsChangedParams {
    /// The watch that saw the change.
    pub watch_id: u64,
    /// The path that changed, which is the watched path itself for a file watch.
    pub path: String,
    /// What happened to it.
    pub kind: FsChangeKind,
}

/// What happened to a watched path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsChangeKind {
    /// The path appeared.
    Created,
    /// The contents changed.
    Modified,
    /// The path went away, by deletion or by being renamed out of the watch.
    Removed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_serializes_with_its_entry_kinds_spelled_out() {
        let result = FsListResult {
            entries: vec![
                DirEntry {
                    name: "src".to_owned(),
                    kind: EntryKind::Directory,
                },
                DirEntry {
                    name: "Cargo.toml".to_owned(),
                    kind: EntryKind::File,
                },
            ],
        };
        let json = serde_json::to_string(&result).expect("listing should serialize");
        assert_eq!(
            json,
            r#"{"entries":[{"name":"src","kind":"directory"},{"name":"Cargo.toml","kind":"file"}]}"#
        );
        assert_eq!(
            serde_json::from_str::<FsListResult>(&json).expect("listing should parse"),
            result
        );
    }

    #[test]
    fn a_change_event_serializes_with_its_kind_spelled_out() {
        let params = FsChangedParams {
            watch_id: 7,
            path: "/tmp/notes.md".to_owned(),
            kind: FsChangeKind::Modified,
        };
        let json = serde_json::to_string(&params).expect("change should serialize");
        assert_eq!(
            json,
            r#"{"watch_id":7,"path":"/tmp/notes.md","kind":"modified"}"#
        );
        assert_eq!(
            serde_json::from_str::<FsChangedParams>(&json).expect("change should parse"),
            params
        );
    }

    #[test]
    fn an_ack_is_an_empty_object() {
        assert_eq!(
            serde_json::to_string(&Ack {}).expect("ack should serialize"),
            "{}"
        );
        assert_eq!(
            serde_json::from_str::<Ack>("{}").expect("ack should parse"),
            Ack {}
        );
    }
}
