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

use crate::server::Event;

const DEBOUNCE: Duration = Duration::from_millis(80);

/// Start watching `path`, sending [`Event::Reload`] on `events_tx` whenever the
/// file's contents may have changed. The returned watcher must be kept alive for the
/// duration of the watch; dropping it stops watching.
pub fn watch_file(path: &Path, events_tx: Sender<Event>) -> notify::Result<RecommendedWatcher> {
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

    std::thread::spawn(move || debounce_loop(raw_rx, events_tx, target));

    Ok(watcher)
}

/// Coalesce raw filesystem events into debounced reload signals.
fn debounce_loop(
    raw_rx: Receiver<notify::Result<notify::Event>>,
    events_tx: Sender<Event>,
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

        if relevant && events_tx.send(Event::Reload).is_err() {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc::{RecvTimeoutError, channel};

    use super::*;
    use crate::testutil::TestDir;

    /// How long to wait for something we expect to arrive.
    const EXPECT: Duration = Duration::from_secs(5);

    /// Watching is tied to the returned watcher's lifetime, which is how the
    /// server swaps documents without leaking the previous file's watch.
    ///
    /// The end of the watch is checked by waiting for the channel to close
    /// rather than by waiting for a reload not to arrive. Dropping the watcher
    /// closes the source the debounce thread reads from, so the thread returns
    /// and its sender goes with it. That makes a stopped watch something the
    /// test observes rather than infers from silence, so a watch that outlives
    /// its watcher fails here instead of passing whenever the timing is kind.
    #[test]
    fn dropping_the_watcher_ends_the_watch() {
        let dir = TestDir::new("watch-drop");
        let file = dir.join("a.md");
        fs::write(&file, "# A\n").unwrap();

        let (events_tx, events_rx) = channel();
        let watcher = watch_file(&file, events_tx).expect("watch the file");

        // The watch is live: a write reaches the channel. A reload for the
        // write above can arrive first, since macOS replays events from just
        // before a stream is created, but either way the next thing on the
        // channel is a reload.
        fs::write(&file, "# A changed\n").unwrap();
        assert_eq!(events_rx.recv_timeout(EXPECT), Ok(Event::Reload));

        drop(watcher);
        fs::write(&file, "# A changed again\n").unwrap();

        // Reloads still in flight from the writes above are fine; what has to
        // happen is that the stream ends.
        loop {
            match events_rx.recv_timeout(EXPECT) {
                Ok(Event::Reload) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("the watch outlived its watcher"),
                Ok(other) => panic!("a file watch should only reload, but sent {other:?}"),
            }
        }
    }
}
