//! Parameters and results of the methods the daemon serves.
//!
//! Paths are the daemon's own, in its own syntax; the crate neither parses nor normalizes them.
//! That holds for the paths a listing hands back as well, so a client walking a directory sends
//! one of them on rather than building a path out of a name and a separator it would have to guess.

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
    /// File name, to show. Not something to build a path out of; that is what `path` is.
    pub name: String,
    /// The whole path to this child, in the daemon's own syntax, ready to be the `path` of an
    /// `fs.read`, `fs.list` or `fs.watch` without anything being joined to it. A client that never
    /// looks inside it needs to know nothing about how the daemon's file system spells a path
    /// (`documents/adr/0004-protocol-envelope-and-listing-contract.md`).
    ///
    /// Absent when the child's path is not valid UTF-8, which a JSON string cannot carry without
    /// changing it: a path with the bad bytes papered over would name some other file or no file
    /// at all, so the child is listed — it is a child all the same — but not addressed. The same
    /// stance `fs.read` takes on content that is not UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
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
    /// A child that is none of the three: a socket, a FIFO, a device. It is in the listing because
    /// a listing names every child of the directory, and it is not something to read or write.
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
                    path: Some("/tmp/src".to_owned()),
                    kind: EntryKind::Directory,
                },
                DirEntry {
                    name: "Cargo.toml".to_owned(),
                    path: Some("/tmp/Cargo.toml".to_owned()),
                    kind: EntryKind::File,
                },
                DirEntry {
                    name: "wim.sock".to_owned(),
                    path: Some("/tmp/wim.sock".to_owned()),
                    kind: EntryKind::Other,
                },
                // A child whose path is not valid UTF-8 is listed without one: no `path` key at
                // all, rather than a path with the bad bytes papered over.
                DirEntry {
                    name: "caf\u{fffd}.md".to_owned(),
                    path: None,
                    kind: EntryKind::File,
                },
            ],
        };
        let json = serde_json::to_string(&result).expect("listing should serialize");
        assert_eq!(
            json,
            r#"{"entries":[{"name":"src","path":"/tmp/src","kind":"directory"},{"name":"Cargo.toml","path":"/tmp/Cargo.toml","kind":"file"},{"name":"wim.sock","path":"/tmp/wim.sock","kind":"other"},{"name":"caf�.md","kind":"file"}]}"#
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
