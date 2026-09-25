//! Helpers shared by the unit tests.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bytes a Unix socket address has for its path, including the terminating
/// NUL: `sun_path` is 104 bytes on macOS and the BSDs, 108 on Linux.
const SUN_PATH_LEN: usize = 104;

/// The longest path a test builds inside its [`TestDir`]. `ensure_socket_dir`
/// is tested with the socket in a subdirectory, which is the deepest case.
const LONGEST_ENTRY: &str = "sub/mdpreview.sock";

/// A canonical base directory that leaves room for those socket paths.
///
/// `TMPDIR` is used when it fits. On macOS it does not: it is a per-user
/// directory under `/var/folders` that canonicalizes to around 57 bytes, which
/// overflows `sun_path` once a test directory and socket name are appended, so
/// binding fails with `InvalidInput: path must be shorter than SUN_LEN`.
/// `/tmp` (`/private/tmp` canonicalized) is the fallback.
fn base_dir(name: &str) -> PathBuf {
    let preferred = canonical(std::env::temp_dir());
    if fits(&preferred, name) {
        return preferred;
    }
    let fallback = canonical(PathBuf::from("/tmp"));
    assert!(
        fits(&fallback, name),
        "no temporary directory is short enough for a Unix socket: \
         tried {} and {}",
        preferred.display(),
        fallback.display(),
    );
    fallback
}

fn canonical(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

/// Whether the longest socket path under `base/name` still fits `sun_path`.
fn fits(base: &Path, name: &str) -> bool {
    base.join(name).join(LONGEST_ENTRY).as_os_str().len() < SUN_PATH_LEN
}

/// A private (mode 0700), canonicalized temporary directory, removed on drop.
pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let child = format!("mdpreview-test-{}-{name}-{n}", std::process::id());
        let path = base_dir(&child).join(&child);
        let _ = fs::remove_dir_all(&path);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("create test dir");
        TestDir(path.canonicalize().expect("canonicalize test dir"))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_dir_leaves_room_for_a_control_socket() {
        let dir = TestDir::new("a-deliberately-long-descriptive-test-name");
        let socket = dir.join(LONGEST_ENTRY);
        assert!(
            socket.as_os_str().len() < SUN_PATH_LEN,
            "{} is {} bytes, too long to bind",
            socket.display(),
            socket.as_os_str().len(),
        );
    }
}
