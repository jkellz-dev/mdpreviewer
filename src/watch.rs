//! File-change watching.
//!
//! Editors typically save by writing a temp file and atomically renaming it
//! over the target, so the target's inode changes and a direct file watch can
//! go stale. To be robust we watch the file's parent directory and filter
//! events down to the target path, with a short debounce to coalesce the
//! burst of events a single save produces.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

const DEBOUNCE: Duration = Duration::from_millis(80);

/// Start watching `path`, sending `()` on `reload_tx` whenever the file's
/// contents may have changed. The returned watcher must be kept alive for the
/// duration of the watch; dropping it stops watching.
pub fn watch_file(path: &Path, reload_tx: Sender<()>) -> notify::Result<RecommendedWatcher> {
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let parent = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let (raw_tx, raw_rx) = channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = raw_tx.send(res);
    })?;
    watcher.watch(&parent, RecursiveMode::NonRecursive)?;

    std::thread::spawn(move || debounce_loop(raw_rx, reload_tx, target));

    Ok(watcher)
}

/// Coalesce raw filesystem events into debounced reload signals.
fn debounce_loop(
    raw_rx: Receiver<notify::Result<notify::Event>>,
    reload_tx: Sender<()>,
    target: PathBuf,
) {
    loop {
        let first = match raw_rx.recv() {
            Ok(event) => event,
            Err(_) => return,
        };
        let mut relevant = event_touches(&first, &target);

        // Drain everything that arrives within the debounce window.
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match raw_rx.recv_timeout(deadline - now) {
                Ok(event) => relevant |= event_touches(&event, &target),
                Err(_) => break,
            }
        }

        if relevant && reload_tx.send(()).is_err() {
            return;
        }
    }
}

/// Decide whether an event refers to our target file. Pure access events (for
/// example reads) are ignored so opening the file in another tool does not
/// trigger a reload.
fn event_touches(event: &notify::Result<notify::Event>, target: &Path) -> bool {
    let Ok(event) = event else {
        return false;
    };
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event
        .paths
        .iter()
        .any(|p| p == target || p.canonicalize().map(|c| c == target).unwrap_or(false))
}
