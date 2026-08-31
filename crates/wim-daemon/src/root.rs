//! The directory the daemon serves, and the one place a request's path becomes a real path.

use std::io;
use std::path::{Component, Path, PathBuf};

use tokio::fs;
use wim_protocol::{ErrorCode, ResponseError};

use crate::io_error;

/// The directory a daemon serves, resolved once so that everything it is compared against holds
/// no symlink and no `..`.
#[derive(Debug)]
pub struct Root {
    path: PathBuf,
}

impl Root {
    /// Anchors a daemon at `path`.
    pub async fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            path: fs::canonicalize(path).await?,
        })
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
