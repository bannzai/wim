//! The directory the daemon serves, and the one place a request's path becomes a real path.

use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use tokio::fs;
use wim_protocol::{ErrorCode, ResponseError};

use crate::io_error;

/// What the daemon's own working files under the root are called.
///
/// The file a write stages its content in and the probe a watch opens with live under the root,
/// because a rename is atomic only within one file system and a watch can only be told about what
/// it watches. Giving them one prefix is what lets them be kept from clients in one place: entries
/// under it are left out of `fs.list` and of what a watch pushes, and a request that names one is
/// refused (`documents/adr/0002-daemon-watch-and-staging-robustness.md`).
pub(crate) const RESERVED_PREFIX: &str = ".wim-";

/// Whether the last component of `path` is a name the daemon keeps for itself.
pub(crate) fn names_reserved(path: &Path) -> bool {
    path.file_name().is_some_and(is_reserved)
}

/// Whether `name` is one the daemon keeps for itself.
pub(crate) fn is_reserved(name: &OsStr) -> bool {
    name.as_encoded_bytes()
        .starts_with(RESERVED_PREFIX.as_bytes())
}

/// The directory a daemon serves: the one name this process looks up in the file system it was
/// started in, and the handle everything a request names is reached through.
#[derive(Debug)]
pub struct Root {
    path: PathBuf,
    /// The open directory a request's path is opened relative to.
    ///
    /// Shared rather than borrowed because the calls it is used for block and are run on the
    /// blocking pool, which keeps what it is handed for as long as the call takes.
    dir: Arc<Dir>,
}

impl Root {
    /// Anchors a daemon at `path`.
    ///
    /// A root that is not a directory is refused here rather than serving: the paths a request
    /// names are read from it, so a regular file as the root would leave `fs.list` and every child
    /// path failing while `fs.read` and `fs.write` on `"."` worked on that one file.
    pub async fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = fs::canonicalize(path).await?;
        if !fs::metadata(&path).await?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{}: a daemon serves a directory", path.display()),
            ));
        }
        // The one place this process reaches into the file system it was started in by name.
        // Every path a request names afterwards is opened relative to the handle this returns, so
        // the authority a connection has is this directory and nothing above it
        // (`documents/adr/0003-daemon-beneath-semantics-with-cap-std.md`).
        let opened = path.clone();
        let dir = blocking(move || Dir::open_ambient_dir(&opened, ambient_authority())).await?;
        Ok(Self {
            path,
            dir: Arc::new(dir),
        })
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path a request names, as a path to be opened under the root's directory.
    ///
    /// Worked out without asking the file system what any name along it points at: a relative path
    /// is read from the root, an absolute one has the root taken off its front, and a path that is
    /// not under the root that way is refused. What the names along it point at is settled by the
    /// open itself, at every component of it, which is what leaves no moment between deciding a
    /// path is inside the root and using it for another process to change the answer
    /// (`documents/adr/0003-daemon-beneath-semantics-with-cap-std.md`).
    ///
    /// The root itself comes back as `.`, which is the path a directory is asked for itself by; an
    /// empty path is not one an open takes.
    pub(crate) fn relative(&self, requested: &str) -> Result<PathBuf, ResponseError> {
        let confined = self.resolve_lexically(requested)?;
        let relative = confined
            .strip_prefix(&self.path)
            .expect("a confined path begins with the root");
        Ok(if relative.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative.to_path_buf()
        })
    }

    /// The path a watch names, worked out without asking the file system what the names on it
    /// point at.
    ///
    /// A watch is on a name rather than on what that name holds: canonicalizing it would watch the
    /// file a link points at, leaving the link's own replacement and removal unreported
    /// (`documents/adr/0002-daemon-watch-and-staging-robustness.md`). Confinement stays what it was
    /// — a path that climbs out of the root lexically is refused — and no link can widen it,
    /// because watching reads nothing: what a watch reports are names under the root.
    pub fn resolve_lexically(&self, requested: &str) -> Result<PathBuf, ResponseError> {
        refuse_reserved(requested)?;
        let asked = Path::new(requested);
        let candidate = if asked.is_absolute() {
            asked.to_path_buf()
        } else {
            self.path.join(asked)
        };
        self.confine(requested, without_relative_components(&candidate))
    }

    /// Runs `operation` on the root's directory, off the threads the runtime answers requests on.
    ///
    /// The calls a directory handle takes block, the way `std::fs`'s do and unlike `tokio::fs`'s,
    /// which puts each of them on the blocking pool by itself. A whole request goes there at once
    /// instead, which is the grain the daemon already works at: one connection's requests are
    /// answered one after another, so a write's staging, filling and renaming have nothing to
    /// interleave with.
    pub(crate) async fn blocking<T, F>(&self, operation: F) -> io::Result<T>
    where
        F: FnOnce(&Dir) -> io::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let dir = Arc::clone(&self.dir);
        blocking(move || operation(&dir)).await
    }

    /// `resolved` itself when it is under the root, and an error when it is not.
    fn confine(&self, requested: &str, resolved: PathBuf) -> Result<PathBuf, ResponseError> {
        if resolved.starts_with(&self.path) {
            Ok(resolved)
        } else {
            Err(escaped(requested))
        }
    }
}

/// A file system error from an operation under the root, as the response the client reads.
///
/// A path that led out of the root while it was being opened is answered the same way a path that
/// leaves it lexically is, so a client cannot tell a symlink out of the root from a `..`: both are
/// paths this daemon does not serve.
pub(crate) fn confined_error(requested: &str, error: io::Error) -> ResponseError {
    if led_outside(&error) {
        escaped(requested)
    } else {
        io_error(requested, error)
    }
}

/// Whether `error` is the refusal of a path that resolved out of the root.
///
/// There is no error kind of its own to match on: cap-std reports it as `PermissionDenied`, which
/// is also what a directory the operating system refuses comes back as. What tells the two apart
/// is that cap-std builds this one itself and no `errno` is behind it, while a refusal from the
/// operating system carries `EACCES` (cap-std 4.0, `cap_primitives::fs::errors::escape_attempt`).
fn led_outside(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied && error.raw_os_error().is_none()
}

/// Runs a blocking file system call on the pool the runtime keeps for them.
async fn blocking<T, F>(operation: F) -> io::Result<T>
where
    F: FnOnce() -> io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    // A blocking task is not cancelled once it has started, so the only thing that ends one
    // without a result is a panic inside it, which belongs to the connection that asked for it.
    tokio::task::spawn_blocking(operation)
        .await
        .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))
}

/// What a request that reaches outside the root is answered with.
///
/// The message says no more than that: whether the path exists out there is not the client's to
/// learn from a daemon that does not serve it.
fn escaped(requested: &str) -> ResponseError {
    ResponseError::new(
        ErrorCode::PermissionDenied,
        format!("{requested}: outside the directory this daemon serves"),
    )
}

/// Nothing when `requested` names something of the client's, and the refusal when it names one of
/// the daemon's own working files.
///
/// Hiding those files from `fs.list` and from what a watch pushes is not enough on its own: a
/// client that could read and write them could plant something at the name the next write stages
/// under, so the names are made untouchable rather than only invisible.
fn refuse_reserved(requested: &str) -> Result<(), ResponseError> {
    if names_reserved(&without_relative_components(Path::new(requested))) {
        return Err(ResponseError::new(
            ErrorCode::PermissionDenied,
            format!("{requested}: a name beginning with {RESERVED_PREFIX} is this daemon's own"),
        ));
    }
    Ok(())
}

/// `path` with its `.` and `..` components worked out, without asking the file system.
///
/// A `..` that would climb past the root of the file system is dropped, the way the file system
/// itself treats it.
fn without_relative_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Read;

    use tempfile::TempDir;

    /// A root directory holding `notes.md`, with `secret.md` next to it, outside the root.
    async fn root() -> (TempDir, Root) {
        let directory = TempDir::new().expect("a temporary directory should be available");
        let path = directory.path().join("root");
        std::fs::create_dir(&path).expect("the root should be created");
        std::fs::write(path.join("notes.md"), "hello\n").expect("the file should be written");
        std::fs::write(directory.path().join("secret.md"), "secret\n")
            .expect("the file should be written");
        let root = Root::new(&path).await.expect("the root should resolve");
        (directory, root)
    }

    /// What reading `path` through the root's directory comes back as, as a client would see it.
    async fn read_through(root: &Root, requested: &str) -> Result<String, ResponseError> {
        let path = root.relative(requested)?;
        root.blocking(move |dir| {
            let mut content = String::new();
            dir.open(&path)?.read_to_string(&mut content)?;
            Ok(content)
        })
        .await
        .map_err(|error| confined_error(requested, error))
    }

    #[tokio::test]
    async fn a_relative_path_is_read_from_the_root() {
        let (_directory, root) = root().await;
        let resolved = root
            .relative("notes.md")
            .expect("a file in the root should resolve");
        assert_eq!(resolved, Path::new("notes.md"));
    }

    #[tokio::test]
    async fn the_root_itself_is_named_by_a_dot_and_by_nothing() {
        let (_directory, root) = root().await;
        for requested in [".", ""] {
            assert_eq!(
                root.relative(requested).expect("the root should resolve"),
                Path::new("."),
                "{requested:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_absolute_path_inside_the_root_resolves_to_what_it_names_under_it() {
        let (_directory, root) = root().await;
        let inside = root.path().join("notes.md");
        let resolved = root
            .relative(&inside.display().to_string())
            .expect("a file in the root should resolve");
        assert_eq!(resolved, Path::new("notes.md"));
    }

    #[tokio::test]
    async fn a_path_that_climbs_out_of_the_root_is_refused() {
        let (directory, root) = root().await;
        let outside = directory.path().join("secret.md").display().to_string();
        for requested in [
            "../secret.md",
            "./../secret.md",
            "sub/../../secret.md",
            &outside,
        ] {
            let error = root
                .relative(requested)
                .expect_err("a path outside the root should be refused");
            assert_eq!(error.code, ErrorCode::PermissionDenied, "{requested}");
        }
    }

    /// An absolute path is confined by the root's name rather than by what it resolves to, so one
    /// that reaches the same directory by another name is not a path this daemon serves
    /// (`documents/adr/0003-daemon-beneath-semantics-with-cap-std.md`).
    #[cfg(unix)]
    #[tokio::test]
    async fn an_absolute_path_that_reaches_the_root_under_another_name_is_refused() {
        let (directory, root) = root().await;
        let link = directory.path().join("another-name");
        std::os::unix::fs::symlink(root.path(), &link).expect("the link should be created");

        let error = root
            .relative(&link.join("notes.md").display().to_string())
            .expect_err("a path that does not begin with the root should be refused");

        assert_eq!(error.code, ErrorCode::PermissionDenied);
    }

    #[tokio::test]
    async fn a_path_that_does_not_exist_yet_resolves_so_that_it_can_be_written() {
        let (_directory, root) = root().await;
        let resolved = root
            .relative("new.md")
            .expect("a file to be created should resolve");
        assert_eq!(resolved, Path::new("new.md"));
    }

    #[tokio::test]
    async fn a_path_under_a_directory_that_is_not_there_is_reported_as_missing() {
        let (_directory, root) = root().await;
        let error = read_through(&root, "nowhere/new.md")
            .await
            .expect_err("a file under a missing directory should be refused");
        assert_eq!(error.code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn a_root_that_is_not_a_directory_is_refused() {
        let (directory, _root) = root().await;
        let file = directory.path().join("secret.md");
        let error = Root::new(&file)
            .await
            .expect_err("a regular file should not be served as a root");
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
    }

    #[tokio::test]
    async fn a_path_naming_one_of_the_daemons_own_files_is_refused() {
        let (_directory, root) = root().await;
        let inside = root
            .path()
            .join(".wim-0123456789abcdef")
            .display()
            .to_string();
        for requested in [
            ".wim-0123456789abcdef",
            "./.wim-0123456789abcdef",
            "src/../.wim-probe-0123456789abcdef",
            &inside,
        ] {
            let error = root
                .relative(requested)
                .expect_err("a name this daemon keeps for itself should be refused");
            assert_eq!(error.code, ErrorCode::PermissionDenied, "{requested}");
            let error = root
                .resolve_lexically(requested)
                .expect_err("a name this daemon keeps for itself should be refused");
            assert_eq!(error.code, ErrorCode::PermissionDenied, "{requested}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_watch_resolves_to_the_name_it_was_given_rather_than_to_what_the_name_points_at() {
        let (directory, root) = root().await;
        let link = root.path().join("link.md");
        std::os::unix::fs::symlink(directory.path().join("secret.md"), &link)
            .expect("the link should be created");

        assert_eq!(
            root.resolve_lexically("link.md")
                .expect("a name in the root should resolve"),
            link,
            "the link is watched, not the file outside the root it points at"
        );
        assert_eq!(
            read_through(&root, "link.md")
                .await
                .expect_err("reading through a link out of the root should be refused")
                .code,
            ErrorCode::PermissionDenied,
            "what may be read is still what is under the root"
        );
    }

    /// The refusal a link out of the root is answered with is the one a `..` out of it gets: the
    /// path is refused at the open rather than before it, and the client is told the same thing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_that_leads_out_of_the_root_is_refused_the_way_a_path_that_leaves_it_is() {
        let (directory, root) = root().await;
        std::os::unix::fs::symlink(
            directory.path().join("secret.md"),
            root.path().join("link.md"),
        )
        .expect("the link should be created");

        let error = read_through(&root, "link.md")
            .await
            .expect_err("reading through a link out of the root should be refused");

        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert_eq!(
            error.message,
            "link.md: outside the directory this daemon serves"
        );
    }

    /// A link is followed for as long as it stays under the root: what is refused is leaving, not
    /// links.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_that_stays_in_the_root_is_read_through() {
        let (_directory, root) = root().await;
        // Relative, because a link whose target is written as an absolute path names a file in the
        // file system the daemon was started in rather than one under the root, and is refused
        // however that path reads.
        std::os::unix::fs::symlink("notes.md", root.path().join("link.md"))
            .expect("the link should be created");

        let content = read_through(&root, "link.md")
            .await
            .expect("a link under the root should be read through");

        assert_eq!(content, "hello\n");
    }

    #[tokio::test]
    async fn a_watch_on_a_path_that_climbs_out_of_the_root_is_refused() {
        let (directory, root) = root().await;
        let outside = directory.path().join("secret.md").display().to_string();
        for requested in ["..", "../secret.md", "sub/../../secret.md", &outside] {
            let error = root
                .resolve_lexically(requested)
                .expect_err("a path outside the root should be refused");
            assert_eq!(error.code, ErrorCode::PermissionDenied, "{requested}");
        }
    }

    #[test]
    fn a_refusal_the_operating_system_made_is_not_read_as_a_path_that_left_the_root() {
        let refused = io::Error::from_raw_os_error(
            // EACCES, which is what a directory the process may not enter comes back as.
            13,
        );
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(!led_outside(&refused));
        assert_eq!(
            confined_error("locked/notes.md", refused).message,
            format!("locked/notes.md: {}", io::Error::from_raw_os_error(13))
        );
    }

    #[test]
    fn relative_components_are_worked_out_without_climbing_past_the_file_system_root() {
        assert_eq!(
            without_relative_components(Path::new("/a/./b/../c")),
            Path::new("/a/c")
        );
        assert_eq!(
            without_relative_components(Path::new("/../../a")),
            Path::new("/a")
        );
    }
}
