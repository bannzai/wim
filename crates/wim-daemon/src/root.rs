//! The directory the daemon serves, and the one place a request's path becomes a real path.

use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

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

/// The directory a daemon serves, resolved once so that everything it is compared against holds
/// no symlink and no `..`.
#[derive(Debug)]
pub struct Root {
    path: PathBuf,
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
        Ok(Self { path })
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path a request names, as a path under the root.
    ///
    /// A relative path is read from the root, and an absolute one is taken as it is; either way
    /// what comes back is inside the root, so a `..` that climbs out of it and a symlink that
    /// points out of it are both refused here rather than in each method.
    pub async fn resolve(&self, requested: &str) -> Result<PathBuf, ResponseError> {
        refuse_reserved(requested)?;
        let asked = Path::new(requested);
        let candidate = if asked.is_absolute() {
            asked.to_path_buf()
        } else {
            self.path.join(asked)
        };
        // `..` is taken out before the file system is asked anything, so that a path that does not
        // exist yet still has a parent worth resolving.
        let candidate = without_relative_components(&candidate);
        match fs::canonicalize(&candidate).await {
            Ok(resolved) => self.confine(requested, resolved),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // `fs.write` names a file that need not exist yet, so the directory that would
                // hold it is what has to be inside the root.
                let (parent, name) = candidate
                    .parent()
                    .zip(candidate.file_name())
                    .ok_or_else(|| escaped(requested))?;
                let parent = fs::canonicalize(parent)
                    .await
                    .map_err(|error| io_error(requested, error))?;
                Ok(self.confine(requested, parent)?.join(name))
            }
            Err(error) => Err(io_error(requested, error)),
        }
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

    /// `resolved` itself when it is under the root, and an error when it is not.
    fn confine(&self, requested: &str, resolved: PathBuf) -> Result<PathBuf, ResponseError> {
        if resolved.starts_with(&self.path) {
            Ok(resolved)
        } else {
            Err(escaped(requested))
        }
    }
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

    #[tokio::test]
    async fn a_relative_path_is_read_from_the_root() {
        let (_directory, root) = root().await;
        let resolved = root
            .resolve("notes.md")
            .await
            .expect("a file in the root should resolve");
        assert_eq!(resolved, root.path().join("notes.md"));
    }

    #[tokio::test]
    async fn the_root_itself_is_named_by_a_dot_and_by_nothing() {
        let (_directory, root) = root().await;
        for requested in [".", ""] {
            assert_eq!(
                root.resolve(requested)
                    .await
                    .expect("the root should resolve"),
                root.path(),
                "{requested:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_absolute_path_inside_the_root_resolves_to_itself() {
        let (_directory, root) = root().await;
        let inside = root.path().join("notes.md");
        let resolved = root
            .resolve(&inside.display().to_string())
            .await
            .expect("a file in the root should resolve");
        assert_eq!(resolved, inside);
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
                .resolve(requested)
                .await
                .expect_err("a path outside the root should be refused");
            assert_eq!(error.code, ErrorCode::PermissionDenied, "{requested}");
        }
    }

    #[tokio::test]
    async fn a_path_that_does_not_exist_yet_resolves_so_that_it_can_be_written() {
        let (_directory, root) = root().await;
        let resolved = root
            .resolve("new.md")
            .await
            .expect("a file to be created should resolve");
        assert_eq!(resolved, root.path().join("new.md"));
    }

    #[tokio::test]
    async fn a_path_under_a_directory_that_is_not_there_is_reported_as_missing() {
        let (_directory, root) = root().await;
        let error = root
            .resolve("nowhere/new.md")
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
                .resolve(requested)
                .await
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
            root.resolve("link.md")
                .await
                .expect_err("reading through a link out of the root should be refused")
                .code,
            ErrorCode::PermissionDenied,
            "what may be read is still what is under the root"
        );
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
