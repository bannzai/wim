//! The watches one connection holds, and the changes they push back to it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::fs;
use tokio::sync::Notify;
use tokio::sync::mpsc::Sender;
use tokio::time::{Instant, timeout_at};
use tokio_tungstenite::tungstenite::Message;
use wim_protocol::{ErrorCode, Event, FsChangeKind, FsChangedParams, ResponseError, ServerPush};

use crate::io_error;
use crate::root::{RESERVED_PREFIX, names_reserved};

/// How many watches one connection may hold at a time.
///
/// Every watch is a watcher of its own — an FSEvents stream, an inotify descriptor, a thread — so
/// a client that never drops one could take the operating system's supply of them. What an editor
/// holds is a watch per open file and one per working directory, which is one or two digits, so
/// this is far above what a session uses and far below what a machine minds
/// (`documents/adr/0002-daemon-watch-and-staging-robustness.md`).
const WATCH_LIMIT: usize = 64;

/// How long a watch probes for readiness before it answers anyway.
///
/// inotify and ReadDirectoryChangesW report from the moment they are registered and FSEvents is
/// sub-second behind at worst, so a watch that has not seen a probe by now is on a machine where
/// events arrive late rather than one where they never arrive: the watch is answered as it is, and
/// what it reports comes late too (`documents/adr/0002-daemon-watch-and-staging-robustness.md`).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long one probe is waited for before another is made.
///
/// A backend that is not watching yet reports nothing about the probe made in front of it and
/// never will, so readiness is found by probing again rather than by waiting longer; the interval
/// is what a run that is already watching pays, and short enough not to be felt while opening a
/// file.
const PROBE_INTERVAL: Duration = Duration::from_millis(50);

/// How many random bytes a probe name is made of. 64 bits, so that two watches starting at once
/// probe through files of their own rather than through one another's.
const PROBE_BYTES: usize = 8;

/// The watches of one connection, each holding the watcher that feeds it.
///
/// A watch belongs to the connection that asked for it: the ids are the connection's own, and
/// dropping this drops every watcher with it, which is how a client going away stops the watches
/// it left behind.
#[derive(Default)]
pub(crate) struct Watches {
    watchers: HashMap<u64, RecommendedWatcher>,
    /// The id the next watch takes. Counting from 1 keeps 0 out of the ids a client is handed.
    next_id: u64,
}

impl Watches {
    /// Starts reporting changes under `path`, and names the watch that reports them.
    ///
    /// `requested` is the path as the client wrote it, so that an error names what it asked for;
    /// `path` is that path resolved under the root, and `directory` says which of the two kinds of
    /// watch it is. `recursive` is a directory's alone: a file has nothing below it.
    ///
    /// It answers only once the watch is live, so that a change made after it is one the client is
    /// told about. `overflowed` is what says the connection has to end: a client that stops
    /// reading fills the outbox, and a watch that then dropped what it could not push would leave
    /// the client holding a file it believes is current (`documents/adr/0002-daemon-watch-and-
    /// staging-robustness.md`).
    pub(crate) async fn start(
        &mut self,
        requested: &str,
        path: &Path,
        recursive: bool,
        directory: bool,
        outgoing: &Sender<Message>,
        overflowed: &Arc<Notify>,
    ) -> Result<u64, ResponseError> {
        if self.watchers.len() >= WATCH_LIMIT {
            return Err(ResponseError::new(
                ErrorCode::InvalidRequest,
                format!(
                    "{requested}: this connection holds {WATCH_LIMIT} watches, which is as many as \
                     it may; drop one with fs.unwatch"
                ),
            ));
        }
        // What the backends watch is directories: macOS reports nothing at all for a stream opened
        // on a file. Watching a file is therefore watching the directory holding it, and reporting
        // only what happens to the file itself.
        let (target, only) = if directory {
            (path, None)
        } else {
            let parent = path.parent().ok_or_else(|| {
                ResponseError::new(
                    ErrorCode::Io,
                    format!("{requested}: has no directory to be watched in"),
                )
            })?;
            (parent, Some(path.to_path_buf()))
        };
        let watch_id = self.next_id + 1;
        let probe = target.join(probe_name());
        let observed = Arc::new(Notify::new());
        let outgoing = outgoing.clone();
        let overflowed = Arc::clone(overflowed);
        let seen = Arc::clone(&observed);
        let probed = probe.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: Result<notify::Event, notify::Error>| {
                // A watch that fails mid-flight has nothing in the protocol to report it with, and
                // an event of a kind the protocol does not name is nothing the client asked for.
                let Ok(event) = event else {
                    return;
                };
                for (path, kind) in changes(event) {
                    // The probe is looked for ahead of the filters the client's watch is made of,
                    // both of which would drop it: it is in the reserved namespace, and a file
                    // watch reports one path that is not this one.
                    if path == probed {
                        seen.notify_one();
                        continue;
                    }
                    if names_reserved(&path) {
                        continue;
                    }
                    if only.as_ref().is_some_and(|only| only != &path) {
                        continue;
                    }
                    let push = ServerPush::new(Event::FsChanged(FsChangedParams {
                        watch_id,
                        path: path.to_string_lossy().into_owned(),
                        kind,
                    }));
                    let push = serde_json::to_string(&push).expect("a push should serialize");
                    // The backend runs this on a thread of its own that must not be left waiting on
                    // a client, so a full outbox is not waited out: the connection is ended, and
                    // the client reconnects and reads again.
                    if outgoing.try_send(Message::text(push)).is_err() {
                        overflowed.notify_one();
                        return;
                    }
                }
            })
            .map_err(|error| watch_error(requested, error))?;
        let mode = if recursive && directory {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        watcher
            .watch(target, mode)
            .map_err(|error| watch_error(requested, error))?;
        self.watchers.insert(watch_id, watcher);
        self.next_id = watch_id;
        wait_until_watching(&probe, &observed).await;
        Ok(watch_id)
    }

    /// Drops a watch. Dropping its watcher is what unregisters it with the operating system.
    ///
    /// A `watch_id` this connection does not hold is not an error: the watch is already gone,
    /// which is the state the client asked for.
    pub(crate) fn stop(&mut self, watch_id: u64) {
        self.watchers.remove(&watch_id);
    }
}

/// Makes changes a fresh watch already knows about until it is told about one.
///
/// A backend answers that it is watching before it is: on macOS the changes made in the window
/// right after an FSEvents stream starts are reported by nobody, so a probe made in that window is
/// not late — it is gone. Probing again is therefore what finds the moment the watch became live,
/// and seeing one come back is what turns the answer to `fs.watch` into a promise that the changes
/// after it are reported.
///
/// Best effort, and both of the ways it gives up are deliberate: a directory that cannot be
/// written in is a directory worth watching all the same, and a window wider than [`PROBE_TIMEOUT`]
/// costs the promise rather than the watch.
async fn wait_until_watching(probe: &Path, observed: &Notify) {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        // Waited on from before the probe is made, so that an event that arrives while it is still
        // being made is one this waits out rather than one it waits for.
        let notified = observed.notified();
        if !probe_once(probe).await {
            return;
        }
        let round = (Instant::now() + PROBE_INTERVAL).min(deadline);
        if timeout_at(round, notified).await.is_ok() || round == deadline {
            return;
        }
    }
}

/// Makes one probe under `probe`, and says whether it could be made at all.
async fn probe_once(probe: &Path) -> bool {
    // `create_new`, so that the name is this probe's own: a file another process left there —
    // a symlink pointing out of the root — is a probe that is skipped, not one written through.
    let created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(probe)
        .await;
    let Ok(file) = created else {
        return false;
    };
    drop(file);
    fs::remove_file(probe).await.is_ok()
}

/// A name for one watch's probe.
///
/// Under the daemon's reserved prefix, so that the client neither sees the probe nor can plant a
/// file at its name.
///
/// Not idempotent, and has to be: two watches starting at once must not probe through one file,
/// where the first probe's removal would answer for the second's readiness.
fn probe_name() -> String {
    let mut bytes = [0u8; PROBE_BYTES];
    getrandom::fill(&mut bytes).expect("the operating system should have a random generator");
    let probe: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{RESERVED_PREFIX}probe-{probe}")
}

/// What one event reports, path by path, and nothing at all for an event the protocol does not
/// name.
///
/// A rename reaches the backends either as the two halves the protocol has or as one event
/// carrying the path it left and the path it arrived at, and the pair is split back into those two
/// halves here: a client told only that both paths were modified would keep the entry that is gone
/// and never learn of the one that is there.
fn changes(event: notify::Event) -> Vec<(PathBuf, FsChangeKind)> {
    use notify::event::{ModifyKind, RenameMode};

    if let EventKind::Modify(ModifyKind::Name(RenameMode::Both)) = event.kind
        && let [from, to] = event.paths.as_slice()
    {
        return vec![
            (from.clone(), FsChangeKind::Removed),
            (to.clone(), FsChangeKind::Created),
        ];
    }
    // A paired rename that does not carry both of its paths says which of them is which no better
    // than any other event does, so it stays what every `Modify` the protocol has no name for is:
    // the paths it does carry changed.
    let Some(kind) = change_kind(&event.kind) else {
        return Vec::new();
    };
    event.paths.into_iter().map(|path| (path, kind)).collect()
}

/// What the protocol calls the change an event reports, and `None` for an event it does not name.
///
/// The halves of a rename are the two the protocol has: the path it left is gone, and the path it
/// arrived at appeared.
fn change_kind(kind: &EventKind) -> Option<FsChangeKind> {
    use notify::event::{ModifyKind, RenameMode};

    match kind {
        EventKind::Create(_) => Some(FsChangeKind::Created),
        EventKind::Remove(_) => Some(FsChangeKind::Removed),
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => Some(FsChangeKind::Removed),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => Some(FsChangeKind::Created),
        EventKind::Modify(_) => Some(FsChangeKind::Modified),
        // `Access` is a read rather than a change, and `Any` and `Other` say too little to
        // normalize into one of the three kinds the protocol has.
        EventKind::Access(_) | EventKind::Any | EventKind::Other => None,
    }
}

/// A watch that could not be set up, as the response the client reading it can match on.
fn watch_error(requested: &str, error: notify::Error) -> ResponseError {
    let message = format!("{requested}: {error}");
    match error.kind {
        notify::ErrorKind::Io(error) => io_error(requested, error),
        notify::ErrorKind::PathNotFound => ResponseError::new(ErrorCode::NotFound, message),
        _ => ResponseError::new(ErrorCode::Io, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use notify::event::{
        AccessKind, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind, RenameMode,
    };

    #[test]
    fn every_kind_the_protocol_names_comes_from_the_event_that_means_it() {
        for (kind, expected) in [
            (EventKind::Create(CreateKind::File), FsChangeKind::Created),
            (EventKind::Remove(RemoveKind::File), FsChangeKind::Removed),
            (
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                FsChangeKind::Modified,
            ),
            (
                EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime)),
                FsChangeKind::Modified,
            ),
            (
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                FsChangeKind::Removed,
            ),
            (
                EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                FsChangeKind::Created,
            ),
        ] {
            assert_eq!(change_kind(&kind), Some(expected), "{kind:?}");
        }
    }

    #[test]
    fn an_event_that_is_not_a_change_is_not_pushed() {
        for kind in [
            EventKind::Access(AccessKind::Read),
            EventKind::Any,
            EventKind::Other,
        ] {
            assert_eq!(change_kind(&kind), None, "{kind:?}");
        }
    }

    #[test]
    fn a_rename_that_arrives_as_one_event_is_reported_as_the_two_halves_it_is_made_of() {
        let event = notify::Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(PathBuf::from("/root/before.txt"))
            .add_path(PathBuf::from("/root/after.txt"));
        assert_eq!(
            changes(event),
            [
                (PathBuf::from("/root/before.txt"), FsChangeKind::Removed),
                (PathBuf::from("/root/after.txt"), FsChangeKind::Created),
            ]
        );
    }

    #[test]
    fn a_rename_that_does_not_carry_both_of_its_paths_is_reported_as_a_change_to_what_it_names() {
        let event = notify::Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(PathBuf::from("/root/only.txt"));
        assert_eq!(
            changes(event),
            [(PathBuf::from("/root/only.txt"), FsChangeKind::Modified)]
        );
    }

    #[test]
    fn an_event_the_protocol_does_not_name_reports_nothing_about_the_paths_it_carries() {
        let event = notify::Event::new(EventKind::Access(AccessKind::Read))
            .add_path(PathBuf::from("/root/notes.txt"));
        assert!(changes(event).is_empty());
    }

    #[test]
    fn a_watch_that_cannot_be_set_up_names_the_path_the_request_asked_for() {
        let error = watch_error(
            "nowhere",
            notify::Error::new(notify::ErrorKind::PathNotFound),
        );
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(error.message.starts_with("nowhere: "), "{}", error.message);
    }

    #[test]
    fn a_probe_is_named_out_of_the_clients_reach_and_no_two_of_them_match() {
        let one = probe_name();
        let other = probe_name();
        assert!(names_reserved(Path::new(&one)), "{one}");
        assert_ne!(one, other);
    }
}
