# Helix Cursor Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the mdpreview browser tab follow Helix. `\ m` and `C-s` scroll the preview to the cursor line. A single running server is reused, and it switches to whichever Markdown file was opened or saved last.

**Architecture:**

- A per-user Unix socket (`control.rs`) carries one-line `open <path> <line>` requests from short-lived CLI invocations to the running server.
- The server turns those requests into `reload` / `scroll` Server-Sent Events.
- comrak's `data-sourcepos` attributes let the browser client map a source line to a rendered block, which it centers and briefly outlines.

**Tech Stack:**

- Rust 2024.
- Crates: comrak 0.55, tiny_http 0.12, notify 8.2, open 5.4, libc (Unix).
- `std::os::unix::net` for the socket.
- Vanilla JS client with mermaid 11.17.2.

**Spec:** `docs/superpowers/specs/2026-09-23-helix-cursor-sync-design.md`

## Global Constraints

- **Version control:**
  - The repo is Jujutsu, colocated with git.
  - Before Task 1, run `jj new` so the plan's change stays docs-only.
  - Each task ends with `jj commit -m "<message>"`, which describes the current change and starts a new empty one.
  - Never add `Co-Authored-By:` or any Claude attribution to descriptions.
  - Never push.
- **No new dependencies**, including dev-dependencies. Tests use `std` only, plus `src/testutil.rs` from Task 3.
- **Tests** are inline `#[cfg(test)]` modules, following the repo's existing convention.
- **Sync mode (`--sync`)** always exits 0 and prints nothing to stdout or stderr, whatever happens.
- **Timeouts:**
  - The client waits 1 s in open mode and 500 ms in sync mode.
  - The server waits 1 s on each control connection.
- **Socket location:**
  - `$XDG_RUNTIME_DIR/mdpreview.sock` when that variable is set, non-empty and absolute.
  - Otherwise `<temp_dir>/mdpreview-<uid>/mdpreview.sock`.
  - The socket's directory must be owned by the user and have no group or other permission bits.
- **Protocol:** `open\t<abs path>\t<line or empty>\n`. The reply is `ok\t<url>\t<clients>\n` or `err\t<reason>\n`.
- **SSE frames** must be exactly `event: reload\ndata:\n\n` and `event: scroll\ndata: <N>\n\n`. Heartbeats stay `: ping\n\n`.
- **Idle exit:** the server exits 15 s after the last `/events` client disconnects, only after the first client has connected. Tests disable this with `idle_grace: None`.
- **Manual runs:** kill only PIDs you started, never `pkill` by name. Pass `--no-open` so no browser tabs open, and set `XDG_RUNTIME_DIR` to a scratch directory so the user's running server is left alone.

## Review Focus

1. **Non-ASCII or space-containing file names** (`café notes.md`) must show correctly in the tab title. They must not break the `X-Mdpreview-File` header. This is pinned by `percent_encode` tests in Task 5.
2. **`C-s` in non-Markdown buffers** must be a silent no-op that never touches the socket. This covers `[scratch]`, `main.rs` and `foo.md.bak`. `README.MD` and `notes.markdown` must still sync. This is pinned by `is_markdown` tests in Task 6.
3. **Re-opening the file that is already current** must scroll without a reload. A reload would re-render every mermaid diagram on every `C-s`. This is pinned by an integration test in Task 5.
4. **A socket directory that is a symlink, or that group/other users can access,** must be refused _before connecting as well as before binding_, so another user can't plant or intercept the socket. That includes `$XDG_RUNTIME_DIR` itself. This is pinned by `ensure_socket_dir` tests in Task 3, and Task 6 calls the check ahead of `send_open` in both modes.
5. **A control client that connects and never sends anything** must not stall the next `C-s` past the server's 1 s timeout. This is pinned by a listener test in Task 3.

---

## File Structure

| File                                                    | Responsibility                                                                      | Tasks        |
| ------------------------------------------------------- | ----------------------------------------------------------------------------------- | ------------ |
| `src/render.rs`                                         | Markdown → HTML, now with `data-sourcepos`                                          | 1            |
| `src/control.rs` (new, Unix)                            | Socket path, protocol, client `send_open`, server `spawn_listener`, socket cleanup  | 2, 3         |
| `src/testutil.rs` (new, test + Unix)                    | `TestDir`, a private temp dir removed on drop                                       | 3            |
| `src/watch.rs`                                          | Sends `Event::Reload` instead of `()`                                               | 4            |
| `src/server.rs`                                         | `Event`, named SSE, current document, `open` handling, `Config`, `X-Mdpreview-File` | 4, 5         |
| `src/main.rs`                                           | CLI parsing, open/sync modes, `--no-open`, binding before fork                      | 5 (small), 6 |
| `assets/app.js`, `assets/app.css`                       | Named events, scroll-to-line, highlight, title, `#line=N`                           | 4 (small), 7 |
| `README.md`, `CLAUDE.md`, `~/.config/helix/config.toml` | Docs and bindings                                                                   | 8            |

---

### Task 1: Emit `data-sourcepos` from the renderer

**Files:**

- Modify: `src/render.rs`

**Interfaces:**

- Consumes: nothing new.
- Produces: rendered HTML where block elements carry `data-sourcepos="L:C-L:C"`, with lines counted from the top of the file (front matter included). Inline elements get it too. The client filters them out in Task 7. The front matter `<details class="frontmatter">` carries the front matter node's own range, for example `data-sourcepos="1:1-4:3"`.

- [ ] **Step 1: Update the tests to expect source positions**

In `src/render.rs`'s `mod tests`, replace the four existing tests with these and add a fifth. The expected strings come from a probe of comrak 0.55:

````rust
    #[test]
    fn front_matter_renders_as_collapsed_yaml_block() {
        let html = render_markdown("---\ntitle: Hello\ntags: [a, b]\n---\n# Body\n");
        assert!(
            html.starts_with("<details class=\"frontmatter\" data-sourcepos=\"1:1-4:3\">"),
            "{html}"
        );
        assert!(html.contains("<code class=\"language-yaml\">title: Hello\ntags: [a, b]</code>"), "{html}");
        // Line numbers count the front matter, matching Helix's cursor line.
        assert!(html.contains("<h1 data-sourcepos=\"5:1-5:6\">Body</h1>"), "{html}");
        // The closing fence must not turn the YAML into a setext heading.
        assert!(!html.contains("<h2"), "{html}");
        assert!(!html.contains("---"), "{html}");
    }

    #[test]
    fn front_matter_is_escaped() {
        let html = render_markdown("---\ndesc: \"<b>&</b>\"\n---\n");
        assert!(html.contains("desc: &quot;&lt;b&gt;&amp;&lt;/b&gt;&quot;"), "{html}");
    }

    #[test]
    fn documents_without_front_matter_are_unchanged() {
        let html = render_markdown("# Title\n\ntext\n");
        assert_eq!(
            html,
            "<h1 data-sourcepos=\"1:1-1:7\">Title</h1>\n<p data-sourcepos=\"3:1-3:4\">text</p>\n"
        );
    }

    #[test]
    fn mid_document_rule_is_not_front_matter() {
        let html = render_markdown("intro\n\n---\n\nmore\n");
        assert!(!html.contains("frontmatter"), "{html}");
        assert!(html.contains("<hr data-sourcepos=\"3:1-3:3\" />"), "{html}");
    }

    #[test]
    fn mermaid_fences_keep_their_source_position() {
        let html = render_markdown("text\n\n```mermaid\nflowchart LR\n```\n");
        assert!(
            html.contains("<pre data-sourcepos=\"3:1-5:3\"><code class=\"language-mermaid\">"),
            "{html}"
        );
    }
````

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test render`
Expected: FAIL. Every test except `front_matter_is_escaped` fails, because no `data-sourcepos` is emitted yet.

- [ ] **Step 3: Enable sourcepos and tag the front matter block**

In `src/render.rs`:

- Change the `use comrak::nodes::NodeValue;` import to `use comrak::nodes::{NodeValue, Sourcepos};`.
- Add, after the `options.render.r#unsafe = true;` block:

```rust
    // Tag block elements with `data-sourcepos="L:C-L:C"` so the client can
    // scroll to the block under the editor's cursor line.
    options.render.sourcepos = true;
```

Replace the front matter call in `render_markdown`:

```rust
    if let Some(node) = root.first_child()
        && let NodeValue::FrontMatter(raw) = &node.data().value
    {
        render_front_matter(raw, node.data().sourcepos, &mut html);
    }
```

Replace `render_front_matter`'s signature and its first `push_str`:

```rust
/// Render raw front matter (delimiter lines included, as comrak stores it) as a
/// collapsed YAML code block, tagged with the front matter's source lines.
fn render_front_matter(raw: &str, sourcepos: Sourcepos, html: &mut String) {
    let yaml = raw
        .trim()
        .strip_prefix(FRONT_MATTER_DELIMITER)
        .and_then(|rest| rest.strip_suffix(FRONT_MATTER_DELIMITER))
        .unwrap_or(raw)
        .trim_matches('\n');

    html.push_str(&format!(
        "<details class=\"frontmatter\" data-sourcepos=\"{}:{}-{}:{}\">",
        sourcepos.start.line, sourcepos.start.column, sourcepos.end.line, sourcepos.end.column
    ));
    html.push_str("<summary>Front matter</summary>\n");
    html.push_str("<pre><code class=\"language-yaml\">");
    escape(html, yaml).expect("writing to a String cannot fail");
    html.push_str("</code></pre></details>\n");
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test render && cargo clippy --all-targets`
Expected: 5 passed. No clippy warnings in `render.rs`.

- [ ] **Step 5: Commit**

```bash
jj commit -m "feat(render): emit data-sourcepos on rendered blocks

Block elements, and the front matter <details>, now carry comrak's
data-sourcepos so the client can map an editor line to a block."
```

---

### Task 2: Control protocol and socket path (pure functions)

**Files:**

- Create: `src/control.rs`
- Modify: `src/main.rs` (module declaration only)

**Interfaces:**

- Consumes: nothing.
- Produces (all `pub`, in `crate::control`, Unix only):
  - `struct Request { pub path: PathBuf, pub line: Option<u32> }`. Derives `Debug, Clone, PartialEq, Eq`.
  - `enum Reply { Ok { url: String, clients: usize }, Err(String) }`. Derives `Debug, Clone, PartialEq, Eq`.
  - `struct ProtocolError(pub &'static str)`. Implements `Display`.
  - `fn format_request(&Request) -> Result<String, ProtocolError>`
  - `fn parse_request(&str) -> Result<Request, ProtocolError>`
  - `fn format_reply(&Reply) -> String`
  - `fn parse_reply(&str) -> Result<Reply, ProtocolError>`
  - `fn socket_path(xdg_runtime_dir: Option<&OsStr>, temp_dir: &Path, uid: u32) -> PathBuf`
  - `fn default_socket_path() -> PathBuf`
  - `fn current_uid() -> u32`

- [ ] **Step 1: Write the module skeleton with failing tests**

Create `src/control.rs`:

````rust
//! Control channel: a per-user Unix socket through which a new `mdpreview`
//! invocation hands its file and cursor line to an already-running server.
//!
//! One request and one reply per connection, each a single tab-separated line:
//!
//! ```text
//! request:  open\t<absolute path>\t<line or empty>\n
//! reply:    ok\t<url>\t<active SSE clients>\n
//!           err\t<reason>\n
//! ```

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

const SOCKET_NAME: &str = "mdpreview.sock";

/// Show `path` and, if given, scroll to 1-based source `line`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub path: PathBuf,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The server's base URL and how many browser tabs are listening.
    Ok { url: String, clients: usize },
    Err(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError(pub &'static str);

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

pub fn format_request(request: &Request) -> Result<String, ProtocolError> {
    todo!()
}

pub fn parse_request(line: &str) -> Result<Request, ProtocolError> {
    todo!()
}

pub fn format_reply(reply: &Reply) -> String {
    todo!()
}

pub fn parse_reply(line: &str) -> Result<Reply, ProtocolError> {
    todo!()
}

pub fn socket_path(xdg_runtime_dir: Option<&OsStr>, temp_dir: &Path, uid: u32) -> PathBuf {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &str, line: Option<u32>) -> Request {
        Request { path: PathBuf::from(path), line }
    }

    #[test]
    fn request_with_line_round_trips() {
        let original = request("/home/me/notes/a b.md", Some(42));
        let wire = format_request(&original).unwrap();
        assert_eq!(wire, "open\t/home/me/notes/a b.md\t42\n");
        assert_eq!(parse_request(&wire), Ok(original));
    }

    #[test]
    fn request_without_line_round_trips() {
        let original = request("/tmp/x.md", None);
        let wire = format_request(&original).unwrap();
        assert_eq!(wire, "open\t/tmp/x.md\t\n");
        assert_eq!(parse_request(&wire), Ok(original));
    }

    #[test]
    fn paths_with_tabs_or_newlines_are_rejected() {
        assert!(format_request(&request("/tmp/a\tb.md", None)).is_err());
        assert!(format_request(&request("/tmp/a\nb.md", None)).is_err());
    }

    #[test]
    fn malformed_requests_are_rejected() {
        for wire in [
            "",
            "open\t/tmp/x.md\t1",        // no trailing newline
            "close\t/tmp/x.md\t1\n",     // unknown verb
            "open\t/tmp/x.md\n",         // missing line field
            "open\t\t1\n",               // empty path
            "open\t/tmp/x.md\tabc\n",    // non-numeric line
            "open\t/tmp/x.md\t1\textra\n",
        ] {
            assert!(parse_request(wire).is_err(), "accepted {wire:?}");
        }
    }

    #[test]
    fn replies_round_trip() {
        let ok = Reply::Ok { url: "http://127.0.0.1:4242/".into(), clients: 2 };
        assert_eq!(format_reply(&ok), "ok\thttp://127.0.0.1:4242/\t2\n");
        assert_eq!(parse_reply(&format_reply(&ok)), Ok(ok));

        let err = Reply::Err("not a file: /x".into());
        assert_eq!(format_reply(&err), "err\tnot a file: /x\n");
        assert_eq!(parse_reply(&format_reply(&err)), Ok(err));
    }

    #[test]
    fn error_reasons_are_kept_on_one_line() {
        let wire = format_reply(&Reply::Err("a\tb\nc".into()));
        assert_eq!(wire, "err\ta b c\n");
    }

    #[test]
    fn malformed_replies_are_rejected() {
        for wire in ["", "ok\thttp://x/\t2", "ok\thttp://x/\n", "ok\thttp://x/\tmany\n", "yes\n"] {
            assert!(parse_reply(wire).is_err(), "accepted {wire:?}");
        }
    }

    #[test]
    fn socket_path_prefers_xdg_runtime_dir() {
        let path = socket_path(Some(OsStr::new("/run/user/1000")), Path::new("/tmp"), 1000);
        assert_eq!(path, PathBuf::from("/run/user/1000/mdpreview.sock"));
    }

    #[test]
    fn socket_path_falls_back_to_private_temp_dir() {
        let expected = PathBuf::from("/tmp/mdpreview-1000/mdpreview.sock");
        assert_eq!(socket_path(None, Path::new("/tmp"), 1000), expected);
        assert_eq!(socket_path(Some(OsStr::new("")), Path::new("/tmp"), 1000), expected);
        assert_eq!(socket_path(Some(OsStr::new("run/user")), Path::new("/tmp"), 1000), expected);
    }
}
````

In `src/main.rs`, add the module declaration above `mod render;`:

```rust
#[cfg(unix)]
mod control;
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test control`
Expected: FAIL. Every test panics with `not yet implemented`. Dead-code warnings for `control` are expected until Task 5.

- [ ] **Step 3: Implement the functions**

Replace the `todo!()` bodies in `src/control.rs`, and add `default_socket_path` and `current_uid` below `socket_path`:

```rust
/// Encode a request. Paths must be UTF-8 and free of tabs and newlines, which
/// would break the line format.
pub fn format_request(request: &Request) -> Result<String, ProtocolError> {
    let path = request
        .path
        .to_str()
        .ok_or(ProtocolError("path is not valid UTF-8"))?;
    if path.contains(['\t', '\n']) {
        return Err(ProtocolError("path contains a tab or newline"));
    }
    let line = request.line.map(|line| line.to_string()).unwrap_or_default();
    Ok(format!("open\t{path}\t{line}\n"))
}

pub fn parse_request(line: &str) -> Result<Request, ProtocolError> {
    let line = line.strip_suffix('\n').ok_or(ProtocolError("missing newline"))?;
    let mut fields = line.split('\t');
    match (fields.next(), fields.next(), fields.next(), fields.next()) {
        (Some("open"), Some(path), Some(line), None) if !path.is_empty() => {
            let line = if line.is_empty() {
                None
            } else {
                Some(line.parse().map_err(|_| ProtocolError("bad line number"))?)
            };
            Ok(Request { path: PathBuf::from(path), line })
        }
        _ => Err(ProtocolError("expected open<TAB>path<TAB>line")),
    }
}

/// Encode a reply. Tabs and newlines in an error reason become spaces.
pub fn format_reply(reply: &Reply) -> String {
    match reply {
        Reply::Ok { url, clients } => format!("ok\t{url}\t{clients}\n"),
        Reply::Err(reason) => format!("err\t{}\n", reason.replace(['\t', '\n'], " ")),
    }
}

pub fn parse_reply(line: &str) -> Result<Reply, ProtocolError> {
    let line = line.strip_suffix('\n').ok_or(ProtocolError("missing newline"))?;
    match line.split_once('\t') {
        Some(("ok", rest)) => {
            let (url, clients) = rest.split_once('\t').ok_or(ProtocolError("bad reply"))?;
            let clients = clients.parse().map_err(|_| ProtocolError("bad client count"))?;
            Ok(Reply::Ok { url: url.to_owned(), clients })
        }
        Some(("err", reason)) => Ok(Reply::Err(reason.to_owned())),
        _ => Err(ProtocolError("bad reply")),
    }
}

/// Where the control socket lives: `$XDG_RUNTIME_DIR/mdpreview.sock` if that is
/// set to an absolute path, else a private per-user directory under `temp_dir`.
pub fn socket_path(xdg_runtime_dir: Option<&OsStr>, temp_dir: &Path, uid: u32) -> PathBuf {
    match xdg_runtime_dir.map(Path::new) {
        Some(dir) if dir.is_absolute() => dir.join(SOCKET_NAME),
        _ => temp_dir.join(format!("mdpreview-{uid}")).join(SOCKET_NAME),
    }
}

/// [`socket_path`] for this process's environment and user.
pub fn default_socket_path() -> PathBuf {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR");
    socket_path(xdg.as_deref(), &std::env::temp_dir(), current_uid())
}

pub fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test control`
Expected: 9 passed.

- [ ] **Step 5: Commit**

```bash
jj commit -m "feat(control): add socket path and line protocol for the control channel"
```

---

### Task 3: Control transport (bind, client, listener, cleanup)

**Files:**

- Modify: `src/control.rs`
- Create: `src/testutil.rs`
- Modify: `src/main.rs` (module declaration only)

**Interfaces:**

- Consumes: Task 2's `Request`, `Reply`, `ProtocolError`, `format_*` / `parse_*`, and `current_uid`.
- Produces (all `pub`, in `crate::control`):
  - `struct ControlSocket { pub listener: UnixListener, pub path: PathBuf, pub inode: u64 }`
  - `enum SendError { NoServer, Timeout, Protocol(ProtocolError), Io(io::Error) }`. Implements `Display`.
  - `fn ensure_socket_dir(socket: &Path, uid: u32) -> io::Result<()>`
  - `fn bind(path: &Path) -> io::Result<ControlSocket>`. It replaces a stale socket file.
  - `fn send_open(socket: &Path, request: &Request, timeout: Duration) -> Result<Reply, SendError>`
  - `fn spawn_listener<F>(listener: UnixListener, handle: F) where F: Fn(Request) -> Reply + Send + 'static`
  - `fn remove_socket_if_ours(path: &Path, inode: u64)`
- Produces, in `crate::testutil` (test builds only): `struct TestDir` with `TestDir::new(name: &str)`, `.path() -> &Path` and `.join(name: &str) -> PathBuf`. The directory has mode 0700, is canonicalized, and is removed on drop.

- [ ] **Step 1: Add the test helper**

Create `src/testutil.rs`:

```rust
//! Helpers shared by the unit tests.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A private (mode 0700), canonicalized temporary directory, removed on drop.
pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("mdpreview-test-{}-{name}-{n}", std::process::id()));
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
```

In `src/main.rs`, add after `mod server;`:

```rust
#[cfg(all(test, unix))]
mod testutil;
```

- [ ] **Step 2: Write the failing transport tests**

Append these tests inside `src/control.rs`'s existing `mod tests`. Also add these `use` lines at the top of the test module, after `use super::*;`. Everything else the tests use (`fs`, `thread`, `Duration`, `BufRead`, `BufReader`, `Write`, `UnixListener`, `UnixStream`, `DirBuilderExt`, `MetadataExt`) comes in through `super::*` once Step 4 has added the module's imports. Don't import those again, or the redundant-import lint may fire.

```rust
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use crate::testutil::TestDir;

    const WAIT: Duration = Duration::from_secs(3);
```

```rust
    #[test]
    fn ensure_socket_dir_creates_a_private_dir() {
        let dir = TestDir::new("ensure-create");
        let socket = dir.join("sub").join(SOCKET_NAME);
        ensure_socket_dir(&socket, current_uid()).unwrap();
        let mode = fs::metadata(dir.join("sub")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn ensure_socket_dir_accepts_an_existing_private_dir() {
        let dir = TestDir::new("ensure-ok");
        ensure_socket_dir(&dir.join(SOCKET_NAME), current_uid()).unwrap();
    }

    #[test]
    fn ensure_socket_dir_rejects_shared_or_foreign_dirs() {
        let dir = TestDir::new("ensure-reject");

        let shared = dir.join("shared");
        fs::DirBuilder::new().mode(0o755).create(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ensure_socket_dir(&shared.join(SOCKET_NAME), current_uid()).is_err());

        let link = dir.join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert!(ensure_socket_dir(&link.join(SOCKET_NAME), current_uid()).is_err());

        // Owned by someone else, as far as the check can tell.
        assert!(ensure_socket_dir(&dir.join(SOCKET_NAME), current_uid() + 1).is_err());
    }

    #[test]
    fn send_open_round_trips_through_the_listener() {
        let dir = TestDir::new("round-trip");
        let socket = dir.join(SOCKET_NAME);
        let control = bind(&socket).unwrap();
        spawn_listener(control.listener, |request| Reply::Ok {
            url: request.path.display().to_string(),
            clients: request.line.unwrap_or(0) as usize,
        });

        let reply = send_open(&socket, &request("/x/a b.md", Some(4)), WAIT).unwrap();
        assert_eq!(reply, Reply::Ok { url: "/x/a b.md".into(), clients: 4 });
    }

    #[test]
    fn send_open_reports_no_server_without_a_socket() {
        let dir = TestDir::new("no-socket");
        let result = send_open(&dir.join(SOCKET_NAME), &request("/x.md", None), WAIT);
        assert!(matches!(result, Err(SendError::NoServer)), "{result:?}");
    }

    #[test]
    fn stale_sockets_report_no_server_and_are_replaced_by_bind() {
        let dir = TestDir::new("stale");
        let socket = dir.join(SOCKET_NAME);
        drop(bind(&socket).unwrap()); // The file stays behind, like after a crash.
        assert!(socket.exists());

        let result = send_open(&socket, &request("/x.md", None), WAIT);
        assert!(matches!(result, Err(SendError::NoServer)), "{result:?}");
        bind(&socket).expect("bind replaces the stale socket");
    }

    #[test]
    fn send_open_times_out_on_a_silent_server() {
        let dir = TestDir::new("silent-server");
        let socket = dir.join(SOCKET_NAME);
        // Never accepted: the connection sits in the backlog, unanswered.
        let _listener = UnixListener::bind(&socket).unwrap();

        let started = Instant::now();
        let result = send_open(&socket, &request("/x.md", None), Duration::from_millis(200));
        assert!(matches!(result, Err(SendError::Timeout)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_silent_client_does_not_wedge_the_listener() {
        let dir = TestDir::new("silent-client");
        let socket = dir.join(SOCKET_NAME);
        let control = bind(&socket).unwrap();
        spawn_listener(control.listener, |_| Reply::Ok { url: "u".into(), clients: 0 });

        // Connects and never sends anything.
        let _idle = UnixStream::connect(&socket).unwrap();
        thread::sleep(Duration::from_millis(50));

        let started = Instant::now();
        let reply = send_open(&socket, &request("/x.md", None), WAIT).unwrap();
        assert_eq!(reply, Reply::Ok { url: "u".into(), clients: 0 });
        // The listener gives up on the idle client after its 1 s timeout.
        assert!(started.elapsed() < Duration::from_millis(2500));
    }

    #[test]
    fn malformed_requests_get_an_error_reply() {
        let dir = TestDir::new("malformed");
        let socket = dir.join(SOCKET_NAME);
        let control = bind(&socket).unwrap();
        spawn_listener(control.listener, |_| unreachable!("handler must not run"));

        let mut stream = UnixStream::connect(&socket).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.write_all(b"hello\n").unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        assert!(reply.starts_with("err\tbad request"), "{reply:?}");
    }

    #[test]
    fn remove_socket_if_ours_spares_a_replacement() {
        let dir = TestDir::new("cleanup");
        let socket = dir.join(SOCKET_NAME);
        let ours = bind(&socket).unwrap();

        // Another server took over the path: a different file (and inode) now
        // sits there. Creating it while ours still exists guarantees a new inode.
        let theirs = dir.join("theirs");
        fs::write(&theirs, "").unwrap();
        let their_inode = fs::metadata(&theirs).unwrap().ino();
        fs::rename(&theirs, &socket).unwrap();

        remove_socket_if_ours(&socket, ours.inode);
        assert!(socket.exists(), "removed another server's socket");

        remove_socket_if_ours(&socket, their_inode);
        assert!(!socket.exists());
    }
```

- [ ] **Step 3: Run the tests and watch them fail**

Run: `cargo test control`
Expected: compile errors, because `bind`, `send_open`, `spawn_listener`, `ensure_socket_dir`, `remove_socket_if_ours` and `SendError` are not defined yet.

- [ ] **Step 4: Implement the transport**

In `src/control.rs`, replace the `use` block at the top with:

```rust
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
```

Add these constants after `SOCKET_NAME`:

```rust
/// How long the server waits on a connected client before giving up on it.
const SERVER_TIMEOUT: Duration = Duration::from_secs(1);

/// Upper bound on a request line: a maximal path plus the framing.
const MAX_REQUEST: u64 = 8 * 1024;
```

Then append this code after `current_uid` and before `#[cfg(test)]`:

```rust
/// A bound control socket, plus what is needed to remove it safely on exit.
pub struct ControlSocket {
    pub listener: UnixListener,
    pub path: PathBuf,
    /// Inode of the socket file at bind time; see [`remove_socket_if_ours`].
    pub inode: u64,
}

#[derive(Debug)]
pub enum SendError {
    /// No socket file, or nothing listening on it (a stale socket).
    NoServer,
    /// A server accepted the connection but did not answer in time.
    Timeout,
    Protocol(ProtocolError),
    Io(io::Error),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::NoServer => f.write_str("no preview server is running"),
            SendError::Timeout => f.write_str("the preview server did not respond"),
            SendError::Protocol(err) => write!(f, "control protocol error: {err}"),
            SendError::Io(err) => write!(f, "control socket error: {err}"),
        }
    }
}

/// Make sure the socket's directory exists and only `uid` can use it, so no
/// other user can plant or intercept the socket. A missing directory is
/// created with mode 0700; an existing one must be a real directory (not a
/// symlink) owned by `uid` with no group or other permissions.
pub fn ensure_socket_dir(socket: &Path, uid: u32) -> io::Result<()> {
    let dir = socket
        .parent()
        .ok_or_else(|| io::Error::other("socket path has no parent directory"))?;
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    let meta = fs::symlink_metadata(dir)?;
    let problem = if !meta.is_dir() {
        "is not a directory"
    } else if meta.uid() != uid {
        "is owned by another user"
    } else if meta.mode() & 0o077 != 0 {
        "is accessible to other users"
    } else {
        return Ok(());
    };
    Err(io::Error::other(format!("{} {problem}", dir.display())))
}

/// Bind the control socket at `path`, replacing any stale socket file a dead
/// server left behind. Call this only after [`send_open`] found no live server.
pub fn bind(path: &Path) -> io::Result<ControlSocket> {
    ensure_socket_dir(path, current_uid())?;
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let listener = UnixListener::bind(path)?;
    let inode = fs::symlink_metadata(path)?.ino();
    Ok(ControlSocket { listener, path: path.to_owned(), inode })
}

/// Send one `open` request to the server listening on `socket` and wait up to
/// `timeout` (per read or write) for its reply.
pub fn send_open(socket: &Path, request: &Request, timeout: Duration) -> Result<Reply, SendError> {
    let message = format_request(request).map_err(SendError::Protocol)?;
    let stream = UnixStream::connect(socket).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => SendError::NoServer,
        _ => SendError::Io(err),
    })?;
    stream.set_read_timeout(Some(timeout)).map_err(SendError::Io)?;
    stream.set_write_timeout(Some(timeout)).map_err(SendError::Io)?;
    (&stream).write_all(message.as_bytes()).map_err(timeout_or_io)?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).map_err(timeout_or_io)?;
    parse_reply(&reply).map_err(SendError::Protocol)
}

fn timeout_or_io(err: io::Error) -> SendError {
    match err.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => SendError::Timeout,
        _ => SendError::Io(err),
    }
}

/// Answer requests on `listener` with `handle`, one connection at a time on a
/// background thread. Each request is only a mutex update and a channel send,
/// so there is nothing to gain from concurrency; the per-connection timeout
/// keeps one silent client from blocking the next.
pub fn spawn_listener<F>(listener: UnixListener, handle: F)
where
    F: Fn(Request) -> Reply + Send + 'static,
{
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = serve_connection(&stream, &handle);
        }
    });
}

fn serve_connection(stream: &UnixStream, handle: &impl Fn(Request) -> Reply) -> io::Result<()> {
    stream.set_read_timeout(Some(SERVER_TIMEOUT))?;
    stream.set_write_timeout(Some(SERVER_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new(stream.take(MAX_REQUEST)).read_line(&mut line)?;
    let reply = match parse_request(&line) {
        Ok(request) => handle(request),
        Err(err) => Reply::Err(format!("bad request: {err}")),
    };
    let mut writer = stream;
    writer.write_all(format_reply(&reply).as_bytes())
}

/// Remove the socket at `path` if it is still the one bound with `inode`. If a
/// racing server has since replaced it, leave that server's socket alone.
pub fn remove_socket_if_ours(path: &Path, inode: u64) {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.ino() == inode) {
        let _ = fs::remove_file(path);
    }
}
```

Note: `stream.take(MAX_REQUEST)` uses `Read for &UnixStream`, so `stream` is borrowed, not consumed. If the compiler resolves `take` to `Iterator::take`, write `Read::take(stream, MAX_REQUEST)` instead.

- [ ] **Step 5: Run the tests and watch them pass**

Run: `cargo test control`
Expected: 18 passed. `a_silent_client_does_not_wedge_the_listener` takes about 1 s.

- [ ] **Step 6: Commit**

```bash
jj commit -m "feat(control): add socket transport with stale-socket and race handling

bind() replaces a stale socket in a private directory, send_open()
distinguishes 'no server' from a hung one, the listener serves one
connection at a time with a 1 s timeout, and remove_socket_if_ours()
compares inodes so a racing server's socket survives."
```

---

### Task 4: Named SSE events (`reload`, `scroll`)

**Files:**

- Modify: `src/server.rs`, `src/watch.rs`, `src/main.rs`, `assets/app.js`

**Interfaces:**

- Consumes: nothing new.
- Produces:
  - `pub enum Event { Reload, Scroll(u32) }` in `crate::server`. It derives `Debug, Clone, Copy, PartialEq, Eq`.
  - `fn sse_frame(Event) -> String` (private).
  - `watch::watch_file(path: &Path, events_tx: Sender<Event>) -> notify::Result<RecommendedWatcher>`.
  - `server::serve(server, file, events_rx: Receiver<Event>)`. This is an interim signature that Task 5 replaces.
  - The client listens for the named `reload` event.

- [ ] **Step 1: Write the failing tests**

Append to `src/server.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::{Event, sse_frame};

    #[test]
    fn reload_is_a_named_event_with_empty_data() {
        assert_eq!(sse_frame(Event::Reload), "event: reload\ndata:\n\n");
    }

    #[test]
    fn scroll_carries_the_line() {
        assert_eq!(sse_frame(Event::Scroll(42)), "event: scroll\ndata: 42\n\n");
    }
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test server`
Expected: compile error, because `Event` and `sse_frame` are not found.

- [ ] **Step 3: Implement `Event` and switch every channel to it**

In `src/server.rs`, add these after the `SSE_HEARTBEAT` constant:

```rust
/// Something every open preview tab should hear about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The current document changed on disk, or another document became current.
    Reload,
    /// Scroll to the block containing this 1-based source line.
    Scroll(u32),
}

/// Encode an event as a named Server-Sent Event.
fn sse_frame(event: Event) -> String {
    match event {
        Event::Reload => "event: reload\ndata:\n\n".to_owned(),
        Event::Scroll(line) => format!("event: scroll\ndata: {line}\n\n"),
    }
}
```

Then make these replacements in `src/server.rs`:

- `type Clients = Arc<Mutex<Vec<Sender<()>>>>;` becomes `type Clients = Arc<Mutex<Vec<Sender<Event>>>>;`
- `pub fn serve(server: Server, file: PathBuf, reload_rx: Receiver<()>)` becomes `pub fn serve(server: Server, file: PathBuf, events_rx: Receiver<Event>)`, and its body's `spawn_dispatcher(reload_rx, ...)` becomes `spawn_dispatcher(events_rx, ...)`.
- Replace `spawn_dispatcher` with:

```rust
/// Fan events from the watcher (and control socket) out to every connected SSE
/// client, pruning clients whose channel has closed.
fn spawn_dispatcher(events_rx: Receiver<Event>, clients: Clients) {
    thread::spawn(move || {
        while let Ok(event) = events_rx.recv() {
            let mut guard = clients.lock().unwrap();
            guard.retain(|tx| tx.send(event).is_ok());
        }
    });
}
```

- In `serve_events`, `let (tx, rx) = channel::<()>();` becomes `let (tx, rx) = channel::<Event>();`, and its loop becomes:

```rust
    loop {
        let frame = match rx.recv_timeout(SSE_HEARTBEAT) {
            Ok(event) => sse_frame(event),
            // Heartbeat comment; also how a dead socket is detected (write fails).
            Err(RecvTimeoutError::Timeout) => ": ping\n\n".to_owned(),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if write_flush(&mut writer, frame.as_bytes()).is_err() {
            break;
        }
    }
```

- Update `serve_events`'s doc comment from "stream reload events" to "stream events".

In `src/watch.rs`:

- Add `use crate::server::Event;` after the `notify` import.
- In `watch_file` and `debounce_loop`, change the parameter to `events_tx: Sender<Event>`, and pass `events_tx` in the `thread::spawn` call.
- Change the send to `if relevant && events_tx.send(Event::Reload).is_err() {`.
- Update `watch_file`'s doc comment to: "Start watching `path`, sending [`Event::Reload`] on `events_tx` whenever the file's contents may have changed."

In `src/main.rs`'s `run_server`, change `channel::<()>()` to `channel::<server::Event>()`.

In `assets/app.js`, replace `events.onmessage = () => loadContent();` with:

```js
events.addEventListener("reload", () => loadContent());
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test && cargo clippy --all-targets`
Expected: all tests pass. That includes the 2 new ones and the render and control tests. There are no new warnings apart from `control`'s dead code.

- [ ] **Step 5: Smoke-test live reload end to end**

```bash
T="$(mktemp -d)"; cp examples/basics.md "$T/t.md"
cargo build -q && target/debug/mdpreview "$T/t.md"   # opens one browser tab
```

This step opens a single browser tab, because `--no-open` only arrives in Task 6. Check two things in that tab:

- Saving `$T/t.md` (for example `echo >> "$T/t.md"`) still reloads it.
- It still exits on its own about 15 s after the tab is closed.

If no browser is available, skip this step. Task 6 Step 6 covers the same check.

- [ ] **Step 6: Commit**

```bash
jj commit -m "feat(server): send named reload/scroll SSE events

The () reload signal becomes an Event enum so the stream can carry
scroll requests too. The client now listens for the named reload event."
```

---

### Task 5: Switchable current document, control handling and `Config`

**Files:**

- Modify: `src/server.rs` (full replacement below), `src/main.rs` (`run_server` only)

**Interfaces:**

- Consumes:
  - `control::{ControlSocket, Request, Reply, spawn_listener, remove_socket_if_ours, bind, send_open}` from Tasks 2 and 3.
  - `watch::watch_file(&Path, Sender<Event>)` from Task 4.
  - `testutil::TestDir` from Task 3.
- Produces:
  - `pub struct server::Config { pub url: String, pub idle_grace: Option<Duration>, #[cfg(unix)] pub control: Option<control::ControlSocket> }`
  - `pub fn server::serve(server: tiny_http::Server, file: PathBuf, config: Config)`. It creates the event channel and the first watcher itself.
  - `/content` responses carry `X-Mdpreview-File: <percent-encoded file name>`.

- [ ] **Step 1: Replace `src/server.rs`**

This replacement already contains the new tests. Write the whole file:

```rust
//! HTTP server: static shell, rendered content, and a live-update event
//! stream. On Unix it also answers `open` requests on the control socket (see
//! `control.rs`), which switch the previewed document and scroll the page.
//!
//! Routes:
//!   GET /                       preview shell (HTML)
//!   GET /content                rendered markdown fragment; the
//!                               `X-Mdpreview-File` header carries the
//!                               percent-encoded file name
//!   GET /events                 Server-Sent Events: `reload` and `scroll`
//!   GET /assets/app.css         page styling
//!   GET /assets/app.js          client (mermaid, live reload, scroll sync)
//!   GET /assets/github-markdown.css   vendored GitHub markdown theme
//!   GET /assets/mermaid.min.js        vendored mermaid.js

use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use notify::RecommendedWatcher;
use tiny_http::{Header, Request, Response, Server};

#[cfg(unix)]
use crate::control;
use crate::{render, watch};

const SHELL_HTML: &str = include_str!("../assets/shell.html");
const APP_CSS: &str = include_str!("../assets/app.css");
const APP_JS: &str = include_str!("../assets/app.js");
const GITHUB_MARKDOWN_CSS: &str = include_str!("../assets/vendor/github-markdown.css");
const MERMAID_JS: &[u8] = include_bytes!("../assets/vendor/mermaid.min.js");

/// How long a `/events` read blocks before emitting a heartbeat. Heartbeats let
/// the server notice a disconnected client on the next write.
const SSE_HEARTBEAT: Duration = Duration::from_secs(10);

/// Something every open preview tab should hear about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The current document changed on disk, or another document became current.
    Reload,
    /// Scroll to the block containing this 1-based source line.
    Scroll(u32),
}

/// Encode an event as a named Server-Sent Event.
fn sse_frame(event: Event) -> String {
    match event {
        Event::Reload => "event: reload\ndata:\n\n".to_owned(),
        Event::Scroll(line) => format!("event: scroll\ndata: {line}\n\n"),
    }
}

/// How [`serve`] runs.
pub struct Config {
    /// The preview's base URL, reported back to `open` requests.
    pub url: String,
    /// Exit this long after the last browser tab closes. `None` never exits,
    /// which tests rely on.
    pub idle_grace: Option<Duration>,
    /// Where to accept `open` requests. `None` runs a standalone server.
    #[cfg(unix)]
    pub control: Option<control::ControlSocket>,
}

/// Registered senders, one per connected SSE client. The dispatcher fans
/// events out to all of them.
type Clients = Arc<Mutex<Vec<Sender<Event>>>>;

/// The document being previewed, and the watch that reloads it.
struct Current {
    path: PathBuf,
    /// Held only to keep the watch alive; replacing it stops the old watch.
    _watcher: Option<RecommendedWatcher>,
}

/// Shared server state passed to each request handler thread.
#[derive(Clone)]
struct State {
    current: Arc<Mutex<Current>>,
    /// Feeds the dispatcher; cloned into each new watcher.
    events_tx: Sender<Event>,
    clients: Clients,
    active: Arc<AtomicUsize>,
    ever_connected: Arc<AtomicBool>,
}

/// Run the preview server until it shuts itself down (see [`spawn_monitor`]).
pub fn serve(server: Server, file: PathBuf, config: Config) {
    let (events_tx, events_rx) = channel::<Event>();
    let watcher = start_watch(&file, &events_tx);
    let state = State {
        current: Arc::new(Mutex::new(Current { path: file, _watcher: watcher })),
        events_tx,
        clients: Arc::new(Mutex::new(Vec::new())),
        active: Arc::new(AtomicUsize::new(0)),
        ever_connected: Arc::new(AtomicBool::new(false)),
    };

    spawn_dispatcher(events_rx, Arc::clone(&state.clients));

    // Answer `open` requests, and remember the socket to remove it on exit.
    #[cfg(unix)]
    let socket = config.control.map(|control::ControlSocket { listener, path, inode }| {
        let handler_state = state.clone();
        let url = config.url.clone();
        control::spawn_listener(listener, move |request| handler_state.open(request, &url));
        (path, inode)
    });

    if let Some(grace) = config.idle_grace {
        spawn_monitor(
            Arc::clone(&state.active),
            Arc::clone(&state.ever_connected),
            grace,
            move || {
                #[cfg(unix)]
                if let Some((path, inode)) = &socket {
                    control::remove_socket_if_ours(path, *inode);
                }
            },
        );
    }

    for request in server.incoming_requests() {
        let state = state.clone();
        thread::spawn(move || handle(request, &state));
    }
}

impl State {
    /// Handle a control-socket `open`: make `path` the current document (if it
    /// is not already) and ask every tab to scroll to `line`.
    #[cfg(unix)]
    fn open(&self, request: control::Request, url: &str) -> control::Reply {
        let control::Request { path, line } = request;
        if !path.is_absolute() {
            return control::Reply::Err(format!("not an absolute path: {}", path.display()));
        }
        if !path.is_file() {
            return control::Reply::Err(format!("not a file: {}", path.display()));
        }
        {
            let mut current = self.current.lock().unwrap();
            if current.path != path {
                let watcher = start_watch(&path, &self.events_tx);
                // Replacing `Current` drops, and so stops, the previous watch.
                *current = Current { path, _watcher: watcher };
                let _ = self.events_tx.send(Event::Reload);
            }
        }
        if let Some(line) = line {
            let _ = self.events_tx.send(Event::Scroll(line));
        }
        control::Reply::Ok {
            url: url.to_owned(),
            clients: self.active.load(Ordering::Relaxed),
        }
    }
}

/// Watch `path`, sending [`Event::Reload`] on changes. A failed watch only
/// disables live reload, so it is reported but not fatal.
fn start_watch(path: &Path, events_tx: &Sender<Event>) -> Option<RecommendedWatcher> {
    match watch::watch_file(path, events_tx.clone()) {
        Ok(watcher) => Some(watcher),
        Err(err) => {
            eprintln!("mdpreview: file watch failed, live reload disabled: {err}");
            None
        }
    }
}

/// Fan events from the watcher and control socket out to every connected SSE
/// client, pruning clients whose channel has closed.
fn spawn_dispatcher(events_rx: Receiver<Event>, clients: Clients) {
    thread::spawn(move || {
        while let Ok(event) = events_rx.recv() {
            let mut guard = clients.lock().unwrap();
            guard.retain(|tx| tx.send(event).is_ok());
        }
    });
}

/// Exit the process once every client has been gone for `grace`, so a closed
/// browser tab does not leave an orphaned server running forever. Only arms
/// after the first client has connected, so the browser has time to open.
/// `on_exit` runs just before the process exits.
fn spawn_monitor(
    active: Arc<AtomicUsize>,
    ever_connected: Arc<AtomicBool>,
    grace: Duration,
    on_exit: impl Fn() + Send + 'static,
) {
    const STEP: Duration = Duration::from_secs(3);
    thread::spawn(move || {
        let mut idle = Duration::ZERO;
        loop {
            thread::sleep(STEP);
            let armed = ever_connected.load(Ordering::Relaxed);
            if armed && active.load(Ordering::Relaxed) == 0 {
                idle += STEP;
                if idle >= grace {
                    on_exit();
                    process::exit(0);
                }
            } else {
                idle = Duration::ZERO;
            }
        }
    });
}

fn handle(request: Request, state: &State) {
    let path = request.url().split('?').next().unwrap_or("/");
    match path {
        "/" => respond(request, SHELL_HTML.as_bytes(), "text/html; charset=utf-8"),
        "/content" => serve_content(request, state),
        "/events" => serve_events(request, state),
        "/assets/app.css" => respond(request, APP_CSS.as_bytes(), "text/css; charset=utf-8"),
        "/assets/app.js" => respond(request, APP_JS.as_bytes(), "text/javascript; charset=utf-8"),
        "/assets/github-markdown.css" => {
            respond(request, GITHUB_MARKDOWN_CSS.as_bytes(), "text/css; charset=utf-8")
        }
        "/assets/mermaid.min.js" => {
            respond(request, MERMAID_JS, "text/javascript; charset=utf-8")
        }
        _ => {
            let response = Response::from_data(&b"not found"[..]).with_status_code(404);
            let _ = request.respond(response);
        }
    }
}

/// Render the current document, with its file name in `X-Mdpreview-File` for
/// the page title. Read errors are reported inline so the browser shows the
/// problem rather than a blank page.
fn serve_content(request: Request, state: &State) {
    // Clone the path so the lock is not held while reading and rendering.
    let path = state.current.lock().unwrap().path.clone();
    let body = match fs::read_to_string(&path) {
        Ok(markdown) => render::render_markdown(&markdown),
        Err(err) => format!(
            "<h1>mdpreview</h1><p>Could not read <code>{}</code>: {}</p>",
            path.display(),
            err
        ),
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let response = Response::from_data(body.into_bytes())
        .with_header(header("Content-Type", "text/html; charset=utf-8"))
        .with_header(header("X-Mdpreview-File", &percent_encode(&name)));
    let _ = request.respond(response);
}

/// Percent-encode a file name for the ASCII-only `X-Mdpreview-File` header;
/// the client decodes it with `decodeURIComponent`.
fn percent_encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Respond with a fixed body and content type.
fn respond(request: Request, body: &[u8], content_type: &str) {
    let response = Response::from_data(body.to_vec()).with_header(header("Content-Type", content_type));
    let _ = request.respond(response);
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

/// Register a new SSE client and stream events to it until it disconnects.
///
/// tiny_http's chunked response writer buffers small writes, which would stall
/// an event stream, so we take the raw socket via `into_writer` and speak the
/// minimal SSE protocol ourselves, flushing after every event.
fn serve_events(request: Request, state: &State) {
    let (tx, rx) = channel::<Event>();
    state.clients.lock().unwrap().push(tx);
    state.ever_connected.store(true, Ordering::Relaxed);
    let _guard = ActiveGuard::new(Arc::clone(&state.active));

    let mut writer = request.into_writer();

    // A bodyless, unbounded response: no Content-Length and no chunked encoding,
    // so the browser reads events incrementally until the socket closes.
    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: text/event-stream\r\n\
                Cache-Control: no-cache\r\n\
                Connection: keep-alive\r\n\
                \r\n";
    if write_flush(&mut writer, head.as_bytes()).is_err() {
        return;
    }
    if write_flush(&mut writer, b": connected\n\n").is_err() {
        return;
    }

    loop {
        let frame = match rx.recv_timeout(SSE_HEARTBEAT) {
            Ok(event) => sse_frame(event),
            // Heartbeat comment; also how a dead socket is detected (write fails).
            Err(RecvTimeoutError::Timeout) => ": ping\n\n".to_owned(),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if write_flush(&mut writer, frame.as_bytes()).is_err() {
            break;
        }
    }
}

fn write_flush(writer: &mut dyn Write, bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(bytes)?;
    writer.flush()
}

/// Tracks the number of connected SSE clients, decrementing on drop so the
/// shutdown monitor can tell when the last client has gone.
struct ActiveGuard(Arc<AtomicUsize>);

impl ActiveGuard {
    fn new(active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::Relaxed);
        ActiveGuard(active)
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, percent_encode, sse_frame};

    #[test]
    fn reload_is_a_named_event_with_empty_data() {
        assert_eq!(sse_frame(Event::Reload), "event: reload\ndata:\n\n");
    }

    #[test]
    fn scroll_carries_the_line() {
        assert_eq!(sse_frame(Event::Scroll(42)), "event: scroll\ndata: 42\n\n");
    }

    #[test]
    fn file_names_are_percent_encoded_for_the_header() {
        assert_eq!(percent_encode("notes-2026_v1.md"), "notes-2026_v1.md");
        assert_eq!(percent_encode("café notes.md"), "caf%C3%A9%20notes.md");
    }
}

/// End-to-end: a real server on a temp socket and a random port, driven the
/// way the CLI and the browser drive it. No fork, no browser, no idle exit.
#[cfg(all(test, unix))]
mod integration_tests {
    use std::fs;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::Duration;

    use super::{Config, serve};
    use crate::control::{self, Reply, Request};
    use crate::testutil::TestDir;

    const WAIT: Duration = Duration::from_secs(5);

    struct Preview {
        http: SocketAddr,
        socket: PathBuf,
    }

    fn start(dir: &TestDir, file: &Path) -> Preview {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let http = listener.local_addr().unwrap();
        let socket = dir.join("mdpreview.sock");
        let config = Config {
            url: format!("http://{http}/"),
            idle_grace: None,
            control: Some(control::bind(&socket).unwrap()),
        };
        let server = tiny_http::Server::from_listener(listener, None).unwrap();
        let file = file.to_owned();
        thread::spawn(move || serve(server, file, config));
        Preview { http, socket }
    }

    fn open(preview: &Preview, path: &Path, line: Option<u32>) -> Reply {
        let request = Request { path: path.to_owned(), line };
        control::send_open(&preview.socket, &request, WAIT).unwrap()
    }

    fn get(preview: &Preview, path: &str) -> String {
        let mut stream = TcpStream::connect(preview.http).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    /// A raw `/events` connection, read frame by frame.
    struct Events {
        stream: TcpStream,
        buf: String,
    }

    impl Events {
        /// Connect and wait until the server has registered this client.
        fn connect(preview: &Preview) -> Self {
            let mut stream = TcpStream::connect(preview.http).unwrap();
            stream.set_read_timeout(Some(WAIT)).unwrap();
            write!(stream, "GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut events = Events { stream, buf: String::new() };
            while !events.buf.contains(": connected\n\n") {
                events.fill();
            }
            let end = events.buf.find(": connected\n\n").unwrap() + ": connected\n\n".len();
            events.buf.drain(..end);
            events
        }

        fn fill(&mut self) {
            let mut chunk = [0u8; 1024];
            let n = self.stream.read(&mut chunk).expect("timed out waiting for an event");
            assert!(n > 0, "event stream closed");
            self.buf.push_str(std::str::from_utf8(&chunk[..n]).unwrap());
        }

        /// The next event frame, skipping `: ping` heartbeats.
        fn next(&mut self) -> String {
            loop {
                if let Some(end) = self.buf.find("\n\n") {
                    let frame: String = self.buf.drain(..end + 2).collect();
                    if !frame.starts_with(':') {
                        return frame;
                    }
                } else {
                    self.fill();
                }
            }
        }
    }

    #[test]
    fn opening_the_current_file_scrolls_without_reloading() {
        let dir = TestDir::new("same-file");
        let a = dir.join("a.md");
        fs::write(&a, "# A\n").unwrap();
        let preview = start(&dir, &a);
        let mut events = Events::connect(&preview);

        let reply = open(&preview, &a, Some(3));
        assert_eq!(reply, Reply::Ok { url: format!("http://{}/", preview.http), clients: 1 });
        assert_eq!(events.next(), "event: scroll\ndata: 3\n\n");

        // No line and no switch: nothing is sent, so the next frame is the
        // scroll from the request after it, not a reload.
        assert!(matches!(open(&preview, &a, None), Reply::Ok { .. }));
        assert!(matches!(open(&preview, &a, Some(5)), Reply::Ok { .. }));
        assert_eq!(events.next(), "event: scroll\ndata: 5\n\n");
    }

    #[test]
    fn opening_another_file_switches_the_document_and_the_watch() {
        let dir = TestDir::new("switch");
        let a = dir.join("a.md");
        let b = dir.join("b.md");
        fs::write(&a, "# A\n").unwrap();
        fs::write(&b, "# B\n").unwrap();
        let preview = start(&dir, &a);
        let mut events = Events::connect(&preview);

        assert!(matches!(open(&preview, &b, Some(1)), Reply::Ok { .. }));
        assert_eq!(events.next(), "event: reload\ndata:\n\n");
        assert_eq!(events.next(), "event: scroll\ndata: 1\n\n");

        let content = get(&preview, "/content");
        // Header name case is up to tiny_http; the value is what matters.
        assert!(content.to_ascii_lowercase().contains("x-mdpreview-file: b.md"), "{content}");
        assert!(content.contains("<h1 data-sourcepos=\"1:1-1:3\">B</h1>"), "{content}");

        // The old file is no longer watched: editing it produces nothing, so
        // the next frame is the scroll we ask for afterwards.
        fs::write(&a, "# A changed\n").unwrap();
        thread::sleep(Duration::from_millis(500));
        assert!(matches!(open(&preview, &b, Some(9)), Reply::Ok { .. }));
        assert_eq!(events.next(), "event: scroll\ndata: 9\n\n");

        // The new file is watched.
        fs::write(&b, "# B changed\n").unwrap();
        assert_eq!(events.next(), "event: reload\ndata:\n\n");
    }

    #[test]
    fn opening_a_missing_file_is_an_error() {
        let dir = TestDir::new("missing");
        let a = dir.join("a.md");
        fs::write(&a, "# A\n").unwrap();
        let preview = start(&dir, &a);

        match open(&preview, &dir.join("nope.md"), None) {
            Reply::Err(reason) => assert!(reason.starts_with("not a file"), "{reason}"),
            reply => panic!("expected an error, got {reply:?}"),
        }
    }

    #[test]
    fn malformed_control_requests_get_an_error_reply() {
        let dir = TestDir::new("malformed-server");
        let a = dir.join("a.md");
        fs::write(&a, "# A\n").unwrap();
        let preview = start(&dir, &a);

        let mut stream = UnixStream::connect(&preview.socket).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.write_all(b"open\trelative.md\n").unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        assert!(reply.starts_with("err\t"), "{reply:?}");
    }
}
```

- [ ] **Step 2: Adapt `run_server` in `src/main.rs`**

In `main`, change `run_server(listener, file);` to `run_server(listener, file, url);`. Then replace `run_server`:

```rust
/// Start the HTTP server on `listener` and serve until the process exits.
fn run_server(listener: TcpListener, file: PathBuf, url: String) {
    let server = match tiny_http::Server::from_listener(listener, None) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("mdpreview: cannot start server: {err}");
            process::exit(1);
        }
    };
    let config = server::Config {
        url,
        idle_grace: Some(std::time::Duration::from_secs(15)),
        #[cfg(unix)]
        control: None,
    };
    server::serve(server, file, config);
}
```

Also remove the now-unused `use std::sync::mpsc::channel;` from `main.rs`.

- [ ] **Step 3: Run the tests and check they pass**

Run: `cargo test && cargo clippy --all-targets`
Expected: every test passes, including the 4 integration tests and the 3 server unit tests. Remaining clippy warnings are limited to `control` items that Task 6 will use (`default_socket_path`, `SendError` display).

If `opening_another_file_switches_the_document_and_the_watch` flakes on the "old file is no longer watched" step, don't just raise the sleep. Check that `Current` really is replaced, which drops the old `RecommendedWatcher`.

- [ ] **Step 4: Commit**

```bash
jj commit -m "feat(server): switch documents and scroll tabs via control requests

serve() now owns the event channel and the current document's watch,
takes a Config (URL, optional idle exit, optional control socket), and
answers open requests: a different file replaces the watch and reloads
every tab; a line sends a scroll event. /content names the file in
X-Mdpreview-File. On idle exit the socket is removed only if its inode
is still ours."
```

---

### Task 6: CLI modes (`--line`, `--sync`, `--no-open`) and server reuse

**Files:**

- Modify: `src/main.rs` (full replacement below)

**Interfaces:**

- Consumes:
  - From Task 3: `control::{default_socket_path, bind, send_open, Request, Reply, SendError, ControlSocket}`.
  - From Task 5: `server::{Config, serve}`.
- Produces: the CLI from the spec. It exits 0 silently in sync mode, and exits 2 with usage for bad arguments in open mode.

- [ ] **Step 1: Replace `src/main.rs` with the new CLI**

This includes the tests:

```rust
//! mdpreview: a live-reloading browser preview for Markdown, with mermaid.
//!
//! Open mode, `mdpreview [--line N] [--no-open] <file>`: if a preview server is
//! already running for this user, tell it (over the control socket, see
//! `control.rs`) to show the file and scroll to the line; otherwise start one.
//! A new server binds a local port, the browser opens at that URL, and the
//! page reloads on save. On Unix the server detaches from the launching
//! process (for example Helix's `:sh`) so the caller returns immediately.
//!
//! Sync mode, `mdpreview --sync [--line N] <file>`: only tell a running server
//! to show the file and scroll. It never starts a server, opens a tab, or
//! prints anything, so it is cheap enough to run on every save.

#[cfg(unix)]
mod control;
mod render;
mod server;
#[cfg(all(test, unix))]
mod testutil;
mod watch;

use std::io::IsTerminal;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

const USAGE: &str = "usage: mdpreview [--line N] [--no-open] <file.md>\n       \
                     mdpreview --sync [--line N] <file.md>";

/// Exit the server this long after the last browser tab closes.
const IDLE_GRACE: Duration = Duration::from_secs(15);

/// How long open mode waits for a running server before giving up.
#[cfg(unix)]
const OPEN_TIMEOUT: Duration = Duration::from_secs(1);

/// How long sync mode waits: it runs on every save, so it must stay snappy.
#[cfg(unix)]
const SYNC_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq, Eq)]
struct Args {
    file: String,
    line: Option<u32>,
    sync: bool,
    no_open: bool,
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(raw.iter().cloned()) {
        Ok(args) => args,
        // Sync mode runs on every save; it never complains.
        Err(_) if raw.iter().any(|arg| arg == "--sync") => process::exit(0),
        Err(msg) => {
            eprintln!("mdpreview: {msg}\n{USAGE}");
            process::exit(2);
        }
    };
    if args.sync {
        run_sync(&args);
        process::exit(0);
    }
    run_open(&args);
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut file = None;
    let mut line = None;
    let mut sync = false;
    let mut no_open = false;
    while let Some(arg) = args.next() {
        if arg == "--sync" {
            sync = true;
        } else if arg == "--no-open" {
            no_open = true;
        } else if arg == "--line" {
            let value = args.next().ok_or("--line needs a value")?;
            line = Some(value.parse().map_err(|_| format!("invalid line number: {value}"))?);
        } else if arg.starts_with("--") {
            return Err(format!("unknown option: {arg}"));
        } else if file.is_some() {
            return Err(format!("unexpected argument: {arg}"));
        } else {
            file = Some(arg);
        }
    }
    let file = file.ok_or("missing file")?;
    Ok(Args { file, line, sync, no_open })
}

/// Sync mode only acts on Markdown files; `C-s` runs it for every buffer.
fn is_markdown(file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

/// The URL to open in a browser. `#line=N` tells a freshly opened tab where to
/// scroll, since it was not connected yet when the editor asked.
fn page_url(url: &str, line: Option<u32>) -> String {
    match line {
        Some(line) => format!("{url}#line={line}"),
        None => url.to_owned(),
    }
}

#[cfg(unix)]
fn run_sync(args: &Args) {
    if !is_markdown(&args.file) {
        return;
    }
    let Ok(path) = Path::new(&args.file).canonicalize() else {
        return;
    };
    let socket = control::default_socket_path();
    // Never talk to a socket in a directory another user could control.
    if control::ensure_socket_dir(&socket, control::current_uid()).is_err() {
        return;
    }
    let request = control::Request { path, line: args.line };
    let _ = control::send_open(&socket, &request, SYNC_TIMEOUT);
}

#[cfg(not(unix))]
fn run_sync(_args: &Args) {}

fn run_open(args: &Args) {
    let file = Path::new(&args.file).canonicalize().unwrap_or_else(|err| {
        eprintln!("mdpreview: cannot open {}: {err}", args.file);
        process::exit(1);
    });

    // Hand the file to a running server if there is one (exits), otherwise
    // claim the control socket for the server we are about to start.
    #[cfg(unix)]
    let control = reuse_or_bind(&file, args);

    // Bind before any fork so the browser can connect via the kernel's accept
    // backlog even before the server thread starts accepting.
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("mdpreview: cannot bind local port: {err}");
            process::exit(1);
        }
    };
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    let url = format!("http://127.0.0.1:{port}/");

    let config = server::Config {
        url: url.clone(),
        idle_grace: Some(IDLE_GRACE),
        #[cfg(unix)]
        control,
    };

    if detach(&url, args) {
        // Only the process that should serve reaches here.
        run_server(listener, file, config);
    }
}

/// Ask a running server to show `file`. If it does, point the browser at it
/// when no tab is listening (or print the URL with `--no-open`) and exit.
/// If no server is running, bind the control socket for a new one; if the
/// socket cannot be used, warn and return `None` for a standalone server.
#[cfg(unix)]
fn reuse_or_bind(file: &Path, args: &Args) -> Option<control::ControlSocket> {
    let socket = control::default_socket_path();
    // Check the directory before connecting: a socket planted by another user
    // could learn our paths and hand back a URL for us to open.
    if let Err(err) = control::ensure_socket_dir(&socket, control::current_uid()) {
        eprintln!("mdpreview: cannot use {} ({err}); running a standalone preview", socket.display());
        return None;
    }
    let request = control::Request { path: file.to_owned(), line: args.line };
    match control::send_open(&socket, &request, OPEN_TIMEOUT) {
        Ok(control::Reply::Ok { url, clients }) => {
            if args.no_open {
                println!("{url}");
            } else if clients == 0 {
                let _ = open::that(page_url(&url, args.line));
            }
            process::exit(0);
        }
        Ok(control::Reply::Err(reason)) => {
            eprintln!("mdpreview: {reason}");
            process::exit(1);
        }
        Err(control::SendError::NoServer) => {}
        Err(err) => {
            eprintln!("mdpreview: {err}");
            process::exit(1);
        }
    }
    match control::bind(&socket) {
        Ok(control) => Some(control),
        Err(err) => {
            eprintln!("mdpreview: cannot use {} ({err}); running a standalone preview", socket.display());
            None
        }
    }
}

/// Detach the server from the launching process.
///
/// On Unix: fork; the parent announces the URL (see [`announce`]) and exits,
/// while the child starts a new session and redirects its standard streams to
/// `/dev/null` (so the launcher's pipe sees EOF). Returns `true` in the process
/// that should run the server.
///
/// On other platforms: announce in-process and run in the foreground.
#[cfg(unix)]
fn detach(url: &str, args: &Args) -> bool {
    match unsafe { libc::fork() } {
        -1 => {
            eprintln!("mdpreview: fork failed");
            process::exit(1);
        }
        0 => {
            detach_child();
            true
        }
        _ => {
            announce(url, args);
            process::exit(0);
        }
    }
}

#[cfg(not(unix))]
fn detach(url: &str, args: &Args) -> bool {
    announce(url, args);
    true
}

/// Open the browser at the preview (scrolled to `--line`) and report the URL.
/// With `--no-open`, only print the URL, even when stdout is not a terminal.
fn announce(url: &str, args: &Args) {
    if args.no_open {
        println!("{url}");
        return;
    }
    let _ = open::that(page_url(url, args.line));
    report_url(url);
}

/// Print the URL only when stdout is a real terminal. When launched from an
/// editor command (for example Helix's `:sh`), stdout is a pipe, so staying
/// silent avoids a captured-output popup; the browser opens regardless.
fn report_url(url: &str) {
    if std::io::stdout().is_terminal() {
        println!("{url}");
    }
}

/// Put the child in its own session and detach its standard streams.
#[cfg(unix)]
fn detach_child() {
    unsafe {
        libc::setsid();
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, libc::STDIN_FILENO);
            libc::dup2(devnull, libc::STDOUT_FILENO);
            libc::dup2(devnull, libc::STDERR_FILENO);
            if devnull > libc::STDERR_FILENO {
                libc::close(devnull);
            }
        }
    }
}

/// Start the HTTP server on `listener` and serve until the process exits.
fn run_server(listener: TcpListener, file: PathBuf, config: server::Config) {
    let server = match tiny_http::Server::from_listener(listener, None) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("mdpreview: cannot start server: {err}");
            process::exit(1);
        }
    };
    server::serve(server, file, config);
}

#[cfg(test)]
mod tests {
    use super::{Args, is_markdown, page_url, parse_args};

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn open_mode_takes_a_file_and_optional_line() {
        assert_eq!(
            parse(&["notes.md"]),
            Ok(Args { file: "notes.md".into(), line: None, sync: false, no_open: false })
        );
        assert_eq!(
            parse(&["--line", "42", "--no-open", "my notes.md"]),
            Ok(Args { file: "my notes.md".into(), line: Some(42), sync: false, no_open: true })
        );
    }

    #[test]
    fn sync_mode_is_a_flag() {
        assert_eq!(
            parse(&["--sync", "--line", "7", "a.md"]),
            Ok(Args { file: "a.md".into(), line: Some(7), sync: true, no_open: false })
        );
    }

    #[test]
    fn bad_arguments_are_rejected() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["--line"]).is_err());
        assert!(parse(&["--line", "x", "a.md"]).is_err());
        assert!(parse(&["--line", "-1", "a.md"]).is_err());
        assert!(parse(&["--bogus", "a.md"]).is_err());
        assert!(parse(&["a.md", "b.md"]).is_err());
    }

    #[test]
    fn only_markdown_files_are_synced() {
        for yes in ["a.md", "docs/README.MD", "notes.markdown", "/abs/x.Md"] {
            assert!(is_markdown(yes), "{yes}");
        }
        for no in ["main.rs", "[scratch]", "foo.md.bak", ".md", "Makefile", ""] {
            assert!(!is_markdown(no), "{no}");
        }
    }

    #[test]
    fn page_url_carries_the_line_as_a_fragment() {
        assert_eq!(page_url("http://127.0.0.1:1/", Some(12)), "http://127.0.0.1:1/#line=12");
        assert_eq!(page_url("http://127.0.0.1:1/", None), "http://127.0.0.1:1/");
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test && cargo clippy --all-targets`
Expected: every test passes, including the 5 new `main` tests. There are no warnings. If clippy flags `control::current_uid` or anything else as dead, it is still in use, so the warning is a sign that something wasn't wired up. Check before silencing anything.

- [ ] **Step 3: Build a release binary for the shell checks**

Run: `cargo build --release`
Expected: success.

- [ ] **Step 4: Check server reuse, sync silence and speed from a shell**

Use a scratch runtime directory so the user's own server is untouched, and `--no-open` so no tab opens. Record the PID you start, and kill only that PID.

```bash
export XDG_RUNTIME_DIR="$(mktemp -d)"; chmod 700 "$XDG_RUNTIME_DIR"
SOCK="$XDG_RUNTIME_DIR/mdpreview.sock"; BIN=target/release/mdpreview
server_pid() { ss -xlpn | grep -F "$SOCK" | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2; }   # the server listening on $SOCK

URL=$($BIN --no-open --line 7 examples/basics.md); echo "$URL"   # starts a server
test -S "$SOCK" && echo "socket ok"
PID=$(server_pid); echo "server pid $PID"

URL2=$($BIN --no-open examples/mermaid.md)
[ "$URL" = "$URL2" ] && echo "reused"
curl -s -D - -o /dev/null "${URL}content" | grep -i '^x-mdpreview-file'    # mermaid.md

OUT=$( { time -p $BIN --sync --line 3 src/main.rs; } 2>&1 ); echo "$OUT"   # no output besides timing; real < 0.05
$BIN --sync --line 3 examples/basics.md; echo "exit $? (expect 0, no output)"
$BIN --sync --line 3 does-not-exist.md; echo "exit $? (expect 0, no output)"
$BIN --no-open does-not-exist.md; echo "exit $? (expect 1 with an error)"
$BIN --bogus x.md; echo "exit $? (expect 2 with usage)"
```

Expected:

- One URL is printed, then "socket ok" and "reused".
- The header line reads `X-Mdpreview-File: mermaid.md`.
- The `.rs` sync prints nothing and takes under 0.05 s real time.
- The two sync calls print nothing and exit 0.
- Open mode on a missing file prints an error and exits 1.
- `--bogus` prints usage and exits 2.

- [ ] **Step 5: Check stale-socket recovery**

```bash
kill -9 "$PID"; test -S "$SOCK" && echo "stale socket left behind"
$BIN --sync --line 1 examples/basics.md; echo "exit $? (expect 0, no output)"
[ -z "$(server_pid)" ] && echo "sync started no server"
URL3=$($BIN --no-open examples/basics.md); echo "$URL3"            # new server, new port
PID=$(server_pid); echo "server pid $PID"
```

Expected:

- "stale socket left behind".
- The sync call is silent and exits 0.
- "sync started no server".
- A new URL, then a new PID.

- [ ] **Step 6: Check the idle exit and socket cleanup**

```bash
curl -sN --max-time 2 "${URL3}events" >/dev/null   # one client connects, then leaves
timeout 25 tail --pid="$PID" -f /dev/null   # waits for the process to exit, at most 25 s
kill -0 "$PID" 2>/dev/null && echo "STILL RUNNING (bad)" || echo "server exited"
test -e "$SOCK" && echo "SOCKET LEFT (bad)" || echo "socket removed"
```

Expected: "server exited" and "socket removed". If the server is still running, kill `$PID` and investigate.

- [ ] **Step 7: Commit**

```bash
jj commit -m "feat(cli): reuse a running server; add --line, --sync and --no-open

Open mode asks the running server (over the control socket) to switch
files and scroll, opening a tab only when none is listening; otherwise
it binds the socket before forking a new server. Sync mode (for C-s)
does the same ask silently and never starts a server. --no-open prints
the URL instead of launching a browser."
```

---

### Task 7: Client scroll-to-line, highlight, title and `#line=N`

**Files:**

- Modify: `assets/app.js` (full replacement below), `assets/app.css` (append)

**Interfaces:**

- Consumes:
  - SSE events `reload` and `scroll` (with `data: <line>`), from Tasks 4 and 5.
  - `data-sourcepos` on blocks, from Task 1.
  - The `X-Mdpreview-File` header (percent-encoded), from Task 5.
  - The `#line=N` URL fragment, from Task 6.
- Produces: user-visible behaviour only.

- [ ] **Step 1: Replace `assets/app.js`**

```js
// mdpreview client: render the markdown fragment, turn fenced mermaid code
// blocks into diagrams, live-reload on Server-Sent Events, and scroll to the
// editor's cursor line when the server asks.

mermaid.initialize({
  startOnLoad: false,
  theme: "dark",
  securityLevel: "loose",
});

const content = document.getElementById("content");

// A scroll requested this soon before a content load started (or while it
// ran) is re-applied once the load has rendered, instead of restoring the old
// scroll position. A save sends `scroll` at once and `reload` ~80ms later when
// the watcher fires; a file switch needs its reload before the target exists;
// and a first render of many diagrams can take longer than this on its own,
// which is why the age is measured from when the load starts.
const PENDING_SCROLL_MS = 1500;

// Elements that make good scroll targets. comrak also puts data-sourcepos on
// inline elements (em, code, a, ...); those are ignored.
const BLOCK_SELECTOR = [
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "p",
  "li",
  "pre",
  "blockquote",
  "table",
  "tr",
  "hr",
  "details",
  "ul",
  "ol",
  "section",
]
  .map((tag) => `${tag}[data-sourcepos]`)
  .join(",");

// The latest scroll request from the editor: { line, at }.
let pendingScroll = null;
// Number of content loads in flight.
let loading = 0;

// Replace each `<pre><code class="language-mermaid">` with a `<pre class="mermaid">`
// element and return the new elements for mermaid to render.
function collectMermaidBlocks() {
  const codes = content.querySelectorAll("pre > code.language-mermaid");
  const blocks = [];
  codes.forEach((code) => {
    const pre = code.parentElement;
    const target = document.createElement("pre");
    target.className = "mermaid";
    // Keep the source position so the diagram stays a scroll target.
    if (pre.dataset.sourcepos) target.dataset.sourcepos = pre.dataset.sourcepos;
    target.textContent = code.textContent;
    pre.replaceWith(target);
    blocks.push(target);
  });
  return blocks;
}

// Parse comrak's `data-sourcepos="L:C-L:C"` into start and end lines.
function sourceLines(el) {
  const match = /^(\d+):\d+-(\d+):\d+$/.exec(el.dataset.sourcepos);
  return match ? { start: Number(match[1]), end: Number(match[2]) } : null;
}

// The block for a 1-based source line: the innermost block containing it,
// else the last block starting before it (blank lines, raw HTML), else null.
function findBlock(line) {
  let innermost = null;
  let innermostSpan = Infinity;
  let before = null;
  for (const el of content.querySelectorAll(BLOCK_SELECTOR)) {
    const lines = sourceLines(el);
    if (!lines) continue;
    if (lines.start <= line) before = el;
    const span = lines.end - lines.start;
    // `<=` so that on a tie the later, more deeply nested element wins.
    if (lines.start <= line && line <= lines.end && span <= innermostSpan) {
      innermost = el;
      innermostSpan = span;
    }
  }
  return innermost ?? before;
}

// Center the block for `line` and briefly outline it.
function scrollToLine(line) {
  const el = findBlock(line);
  if (!el) {
    window.scrollTo({ top: 0, behavior: "smooth" });
    return;
  }
  el.scrollIntoView({ block: "center", behavior: "smooth" });
  el.classList.remove("mdpreview-target");
  void el.offsetWidth; // Force a reflow so a repeated highlight restarts.
  el.classList.add("mdpreview-target");
  el.addEventListener(
    "animationend",
    () => el.classList.remove("mdpreview-target"),
    { once: true },
  );
}

function requestScroll(line) {
  pendingScroll = { line, at: Date.now() };
  // A load in progress applies it once rendered (see loadContent).
  if (loading === 0) scrollToLine(line);
}

async function loadContent() {
  const startedAt = Date.now();
  const scrollY = window.scrollY;
  loading += 1;
  try {
    let response;
    let html;
    try {
      response = await fetch("/content", { cache: "no-store" });
      html = await response.text();
    } catch (err) {
      console.error("mdpreview: failed to fetch content", err);
      return;
    }

    const name = response.headers.get("X-Mdpreview-File");
    if (name) document.title = `${decodeURIComponent(name)} — mdpreview`;

    content.innerHTML = html;

    const blocks = collectMermaidBlocks();
    if (blocks.length > 0) {
      try {
        await mermaid.run({ nodes: blocks });
      } catch (err) {
        console.error("mdpreview: mermaid render failed", err);
      }
    }

    // Diagrams have their final size now, so the target's position is right.
    if (pendingScroll && pendingScroll.at >= startedAt - PENDING_SCROLL_MS) {
      scrollToLine(pendingScroll.line);
    } else {
      window.scrollTo(0, scrollY);
    }
  } finally {
    loading -= 1;
  }
}

// A freshly opened tab gets its first target as `#line=N`, since it was not
// connected yet when the editor asked. Drop the hash afterwards so a manual
// page reload does not jump back.
function takeLineFromHash() {
  const match = /^#line=(\d+)$/.exec(location.hash);
  if (!match) return;
  pendingScroll = { line: Number(match[1]), at: Date.now() };
  history.replaceState(null, "", location.pathname + location.search);
}

takeLineFromHash();
document.addEventListener("DOMContentLoaded", loadContent);

// Live updates. EventSource reconnects automatically if the connection drops.
const events = new EventSource("/events");
events.addEventListener("reload", () => loadContent());
events.addEventListener("scroll", (event) => {
  const line = Number(event.data);
  if (Number.isInteger(line) && line > 0) requestScroll(line);
});
events.onerror = () => {
  // Transient during reconnect; nothing to do.
};
```

- [ ] **Step 2: Append the highlight style to `assets/app.css`**

```css
/* Scroll target: a brief outline around the block under the editor's cursor. */
.markdown-body .mdpreview-target {
  /* The vendored dark theme's accent blue. */
  outline: 2px solid #4493f8;
  outline-offset: 4px;
  animation: mdpreview-target-fade 1.2s ease-out forwards;
}

@keyframes mdpreview-target-fade {
  from {
    outline-color: #4493f8;
  }
  to {
    outline-color: transparent;
  }
}
```

- [ ] **Step 3: Check syntax and rebuild**

Run:

- `command -v node >/dev/null && node --check assets/app.js`. Expected: no output, or node isn't installed.
- `cargo test && cargo build --release`. Expected: pass. The assets are embedded, so the rebuild is required.

- [ ] **Step 4: Check the served client from a shell**

```bash
export XDG_RUNTIME_DIR="$(mktemp -d)"; chmod 700 "$XDG_RUNTIME_DIR"
SOCK="$XDG_RUNTIME_DIR/mdpreview.sock"; BIN=target/release/mdpreview
server_pid() { ss -xlpn | grep -F "$SOCK" | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2; }
URL=$($BIN --no-open examples/mermaid.md); PID=$(server_pid)
curl -s "${URL}assets/app.js" | grep -c 'addEventListener("scroll"'        # 1
curl -s "${URL}content" | grep -o 'data-sourcepos' | wc -l                  # > 27
timeout 3 curl -sN "${URL}events" &
timeout 0.5 tail -f /dev/null   # a 0.5 s pause so the stream connects first
$BIN --sync --line 110 examples/mermaid.md; wait
kill "$PID"
```

Expected:

- `1`.
- A count well above 27.
- The `/events` output shows `event: scroll` / `data: 110`.

- [ ] **Step 5: Ask the user to check the browser behaviour**

The executor can't observe a browser. Hand these checks to the user, and note which ones pass:

1. `target/release/mdpreview --line 110 examples/mermaid.md` opens a tab. It scrolls to the Block diagram, centered, with a blue outline that fades out.
2. `target/release/mdpreview --sync --line 300 examples/mermaid.md` scrolls the same tab to the Railroad section without a reload flash.
3. `target/release/mdpreview --sync --line 20 examples/basics.md` switches the tab to basics.md, centers the "Press Ctrl + C" paragraph, and changes the tab title to `basics.md — mdpreview`.
4. `--sync --line 3` on `examples/front-matter.md` centers the collapsed "Front matter" block.
5. After closing the tab, the server exits and removes its socket within about 20 s.

- [ ] **Step 6: Commit**

```bash
jj commit -m "feat(client): scroll to and highlight the editor's cursor line

Scroll events center the innermost block whose data-sourcepos contains
the line and flash an outline. A scroll that arrives with, or shortly
before, a reload is re-applied after mermaid finishes, so layout shifts
don't misplace it. New tabs take their first line from #line=N, and
the tab title follows the previewed file."
```

---

### Task 8: Docs and Helix bindings

**Files:**

- Modify: `README.md`, `CLAUDE.md`
- Modify (outside the repo, the user's config): `~/.config/helix/config.toml`

**Interfaces:**

- Consumes: the finished CLI.
- Produces: the docs and the working editor integration.

- [ ] **Step 1: Update `README.md`**

Replace the `## Usage` section, up to (but not including) `## Building and installing`, with:

````markdown
## Usage

```sh
mdpreview [--line N] [--no-open] path/to/file.md   # open a preview (or reuse one)
mdpreview --sync [--line N] path/to/file.md        # update a running preview only
```

The first run binds a random local port, opens your default browser at that
URL, and renders the file. Saving the file reloads the content in place. On
Unix the server detaches from its launcher, so it can be run from an editor
command without blocking, and it shuts itself down shortly after the last
browser tab closes.

Only one preview server runs per user. Later runs hand their file to it over a
Unix socket (`$XDG_RUNTIME_DIR/mdpreview.sock`) instead of starting another, so
the open tab switches to the new file. A new tab opens only if none is open.

- `--line N` scrolls the preview to source line `N`, centering and briefly
  highlighting that block.
- `--sync` only talks to a running server. It never starts one, never opens a
  tab and never prints anything, so it is cheap enough to run on every save.
  It ignores files that aren't `.md` or `.markdown`.
- `--no-open` prints the URL instead of opening a browser.

### Helix

```toml
[editor]
# Save shortly after edits, so the preview follows typing, not just C-s.
auto-save = { focus-lost = true, after-delay.enable = true, after-delay.timeout = 300 }

[keys.normal]
"C-s" = [":w", ':sh mdpreview --sync --line %{cursor_line} "%{buffer_name}"']

[keys.insert]
"C-s" = ["normal_mode", ":w", ':sh mdpreview --sync --line %{cursor_line} "%{buffer_name}"']

[keys.normal."\\"]
m = { command = ':sh mdpreview --line %{cursor_line} "%{buffer_name}"', label = "Markdown preview" }
```

`%{buffer_name}` is relative to Helix's working directory, which is also where
`:sh` runs. Helix has no absolute-path variable.
````

In `## How it works`, replace the bullets with:

```markdown
- `render.rs` renders Markdown to HTML with [comrak](https://github.com/kivikakk/comrak)
  (GFM extensions; raw HTML passed through for fidelity; `data-sourcepos` on
  blocks for scroll sync). Fenced `mermaid` blocks are left as code blocks and
  turned into diagrams by the client.
- `watch.rs` watches the file's parent directory (to survive editor
  atomic-rename saves) and emits debounced reload events.
- `server.rs` serves the shell page, the rendered fragment (`/content`), an SSE
  stream of `reload` and `scroll` events (`/events`), and the embedded assets.
  It switches documents when asked over the control socket.
- `control.rs` is the control socket: its location, a one-line protocol, the
  client used by later runs, and the server-side listener.
- `main.rs` parses the CLI, reuses a running server when there is one, and
  otherwise binds the port and socket before forking and runs the server in
  the detached child.
```

- [ ] **Step 2: Update `CLAUDE.md`**

Make these edits:

- **Overview:** replace ``(`:sh mdpreview "%{file_path_absolute}"`)`` with ``(`:sh mdpreview --line %{cursor_line} "%{buffer_name}"`; `C-s` also runs `mdpreview --sync …` to scroll a running preview)``. Replace the "Reload only when the file is written" bullet with:

  ```markdown
  - **Reload only when the file is written.** Helix has no hook for unsaved buffer changes, so the preview updates when the file is saved (Helix's auto-save makes that frequent). That's why reloads are driven by a filesystem watcher.
  - **One server, driven by later invocations.** Helix can only run commands, so later `mdpreview` runs pass the file and cursor line to the running server over a Unix socket. `--sync` mode (bound to `C-s`) must stay silent and fast because it runs on every save of every file.
  ```

- **Commands:** replace "Tests are inline `#[cfg(test)]` modules (currently only `render.rs`)." with "Tests are inline `#[cfg(test)]` modules. `server.rs` has in-process integration tests (real server, temp socket, `idle_grace: None`). `src/testutil.rs` has `TestDir`." Also replace the manual-run sentence's "Kill stray servers with `pkill mdpreview`." with "Use `--no-open` to get the URL without opening a tab, and a scratch `XDG_RUNTIME_DIR` to avoid reusing your real server. Kill test servers by PID (`ss -xlpn | grep mdpreview.sock` shows it)."

- **Architecture:** change "across the four modules" to "across the five modules". Then replace items 1–3 with:

  ```markdown
  1. **`main.rs`** parses `[--line N] [--no-open] [--sync] <file>`. Open mode first asks a running server over the control socket to switch to the file. If one answers, it exits, opening a tab only when no client is connected. Otherwise it binds the control socket and `127.0.0.1:0` _before_ forking, so the browser opened by the parent (at `<url>#line=N`) can connect through the kernel accept backlog before the child's server loop starts. The child calls `setsid()` and redirects stdio to `/dev/null`, so the launching editor's pipe sees EOF. Sync mode only sends the request (500 ms timeout) and always exits 0 silently. On non-Unix platforms it runs in the foreground and `--sync` is a no-op.
  2. **`control.rs`** (Unix) handles the socket:
     - Its path: `$XDG_RUNTIME_DIR/mdpreview.sock`, or a 0700 `mdpreview-<uid>` dir in the temp dir.
     - The one-line protocol: `open\t<path>\t<line>\n` → `ok\t<url>\t<clients>\n` or `err\t<reason>\n`.
     - `send_open`, which tells `NoServer` apart from `Timeout`.
     - A single-threaded listener with a 1 s per-connection timeout.
     - `remove_socket_if_ours`, which removes the socket only if its inode matches the one recorded at bind, so a racing server's socket survives.
  3. **`watch.rs`** watches the file's _parent directory_, not the file itself, so editors that save via atomic rename don't leave a stale inode watch. It filters events down to the target path, ignores `Access` events, debounces bursts (80ms), and sends `Event::Reload` on an mpsc channel.
  4. **`server.rs`** (tiny_http, one thread per request):
     - `serve(server, file, Config { url, idle_grace, control })` owns the event channel and the current document: a `Current { path, watcher }` behind a mutex. A control `open` for a different file replaces it (dropping the old watch) and sends `Event::Reload`. A line sends `Event::Scroll(n)`.
     - A dispatcher thread fans each `Event` out to per-client `Sender`s, one per open `/events` SSE connection, as named events (`event: reload` / `event: scroll` + `data: <line>`).
     - `/events` bypasses tiny_http's buffered chunked writer (`request.into_writer()`) and writes raw SSE with a flush after every event. A 10s heartbeat lets it detect dead sockets.
     - A monitor thread exits the process once the active SSE count (tracked by the `ActiveGuard` RAII type) has been 0 for `idle_grace` (15s), removing the socket first. It only arms after the first client has connected. `idle_grace: None` disables it for tests.
     - `/content` re-reads and re-renders the current file on every request, with no caching, and names it in `X-Mdpreview-File` (percent-encoded).
  ```

  Renumber the existing `render.rs` item to 5 and append to it: "`render.sourcepos` is on, so blocks carry `data-sourcepos=\"L:C-L:C\"` (inline elements too); the front matter `<details>` gets its node's range."

- **Client paragraph:** replace it with:

  ```markdown
  **Client (`assets/app.js`)**: on load and on every `reload` event, it:

  - Fetches `/content`, swaps it into `#content` and sets the title from `X-Mdpreview-File`.
  - Converts `code.language-mermaid` blocks to `<pre class="mermaid">`, keeping `data-sourcepos`, and awaits `mermaid.run`.
  - Then either restores the scroll position or re-applies a recent scroll request.

  On `scroll` events and `#line=N`, `findBlock` picks the innermost block-level element whose sourcepos contains the line (else the last one before it), centers it and flashes a `.mdpreview-target` outline. A scroll requested up to 1.5s before a load started is re-applied after that load renders, because a save sends `scroll` at once and `reload` ~80ms later. Mermaid rendering happens entirely in the browser.
  ```

- [ ] **Step 3: Update the Helix bindings**

Edit `~/.config/helix/config.toml`. It lives outside the repo, and the user approved these bindings in the spec:

- Line 99: `"C-s" = ":w" # Save (was save_selection).` becomes `"C-s" = [":w", ':sh mdpreview --sync --line %{cursor_line} "%{buffer_name}"'] # Save, then scroll a running mdpreview to the cursor (silent no-op otherwise).`
- Line 138: `"C-s" = ["normal_mode", ":w"]` becomes `"C-s" = ["normal_mode", ":w", ':sh mdpreview --sync --line %{cursor_line} "%{buffer_name}"']`. Keep its trailing comment.
- Line 175, the `m = …` entry in `[keys.normal."\\"]`, becomes `m = { command = ':sh mdpreview --line %{cursor_line} "%{buffer_name}"', label = "Markdown preview" } # buffer_name is cwd-relative; :sh runs in the same cwd`.

Then run `mise run install`. This rebuilds the release binary, and `~/.local/bin/mdpreview` is a symlink to it.

- [ ] **Step 4: Hand the manual Helix checklist to the user**

The executor can't drive Helix. Ask the user to run these checks and report back:

1. `\ m` in `examples/basics.md` opens a tab scrolled to the cursor, with the highlight.
2. `C-s` in `examples/mermaid.md` switches the tab to it and updates the title.
3. `C-s` in a `.rs` buffer does nothing visible: no popup, no delay.
4. `C-s` below a mermaid diagram lands on the right block once the diagrams render.
5. Closing the tab makes the server exit and remove its socket within about 20 s.
6. After `kill -9` of the server (PID from `ss -xlpn | grep mdpreview.sock`), the next `\ m` starts a fresh preview.

- [ ] **Step 5: Commit**

Only repo files go in this change; the Helix config lives outside the repo.

```bash
jj commit -m "docs: document scroll sync, server reuse and the Helix bindings"
```

---

## Spec coverage

| Spec section                                                                                                                          | Task                                   |
| ------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------- |
| CLI flags, `parse_args`, usage and exit codes                                                                                         | 6                                      |
| Open mode: reuse, open a tab only when there are 0 clients, `--no-open`, stale-socket recovery, bind before fork, standalone fallback | 3 (`bind`), 6                          |
| Sync mode: extension check, silence, 500 ms, never starts a server                                                                    | 6                                      |
| Non-Unix behaviour                                                                                                                    | 6 (`cfg` fallbacks)                    |
| Socket path and `ensure_socket_dir`                                                                                                   | 2, 3                                   |
| Protocol and pure functions                                                                                                           | 2                                      |
| Listener one connection at a time, 1 s timeout, `err` for malformed requests                                                          | 3                                      |
| `Event`, named SSE                                                                                                                    | 4                                      |
| `Current`, `open` handling, watcher swap, `X-Mdpreview-File`                                                                          | 5                                      |
| Lifetime, inode-checked cleanup, `Config { idle_grace }`                                                                              | 3 (`remove_socket_if_ours`), 5         |
| Render sourcepos and front matter range                                                                                               | 1                                      |
| Client: named listeners, mermaid sourcepos copy, `findBlock`, highlight, `pendingScroll`, `#line=N`, title                            | 4, 7                                   |
| Helix bindings                                                                                                                        | 8                                      |
| Error-handling table                                                                                                                  | 3, 5, 6 (tests and shell checks)       |
| Testing section                                                                                                                       | 1–7, plus the manual checks in 7 and 8 |
| Docs to update                                                                                                                        | 8                                      |
