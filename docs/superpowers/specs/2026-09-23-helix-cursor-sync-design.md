# Helix Cursor Sync and a Reusable Preview Server

Date: 2026-09-23
Status: design approved in chat; awaiting spec review

## Goal

When editing Markdown in Helix, the browser preview should follow the editor:

1. **Scroll to the cursor.** Opening the preview (`\ m`) or saving (`C-s`)
   scrolls the preview so the block under Helix's cursor sits in the middle of
   the viewport, briefly highlighted.
2. **One server, one tab.** A running mdpreview server is reused. Opening the
   preview or saving a *different* Markdown file switches the existing tab to
   that file instead of starting a second server and tab.

### Constraints (from the Helix Use Case)

- Helix has no plugin system or event hooks. The only integration points are
  key bindings that run `:sh` commands with expansion variables
  (`%{cursor_line}`, `%{buffer_name}`). Helix has no `%{file_path_absolute}`,
  and `%{buffer_name}` is relative to the cwd. `:sh` runs in that same cwd.
- `:sh` blocks Helix until the command exits and shows any output in a popup.
  The `C-s` path runs on **every save of every file**, so it has to be fast
  and silent, and it must never open a tab.
- The existing guarantees stay: the command never blocks the editor, stays
  quiet, reloads on file writes, and the server exits once no tab is open.

### Non-Goals

- Syncing unsaved buffer contents. Auto-save (below) covers this.
- Browser → editor sync (clicking in the preview to move the cursor).
- Several simultaneous previews. There is one server per user and one current
  document.
- A control channel on non-Unix platforms.

## Prerequisite: Helix Configuration (Already Applied)

In `~/.config/helix/config.toml`:

- `auto-save = { focus-lost = true, after-delay.enable = true, after-delay.timeout = 300 }`.
- The `\ m` binding uses `%{buffer_name}`. The invalid `%{file_path_absolute}`
  is gone.

The bindings change again once this feature lands (see
[Helix bindings](#helix-bindings)).

## CLI

```
mdpreview [--line N] [--no-open] <file>      # open mode
mdpreview --sync --line N <file>             # sync mode
```

Arguments are parsed by hand (no clap) in a pure function
`parse_args(impl Iterator<Item = String>) -> Result<Args, Usage>` so it can be
unit tested. Unknown flags or a missing file print usage and exit 2 in open
mode. In sync mode, every error exits 0 silently.

- `--line N`: the 1-based source line to scroll to. Helix's `%{cursor_line}`
  counts front matter lines, and so does comrak's sourcepos, so the two match.
- `--no-open`: never launch a browser. Print the URL to stdout
  unconditionally, even when stdout is not a TTY. It exists for testing and for
  scripted use, and it stops test runs from opening stray tabs.
- `--sync`: talk to a running server only (see below).

### Open Mode

1. Canonicalize `<file>`. If that fails, print to `stderr` and exit 1, as today.
2. Try `control::send_open(socket, path, line)` with a **1 s** timeout.
   - **Server replies `ok\t<url>\t<clients>`:** if `clients == 0` and
     `--no-open` is absent, open `<url>#line=N` in the browser. With
     `--no-open`, print `<url>` instead. Otherwise do nothing. Exit 0.
   - **Server replies `err\t<reason>`:** print the reason to stderr and exit 1.
   - **Socket missing, or connection refused (stale socket):** remove the
     stale socket file and start a server (step 3).
   - **Timeout (server hung):** print to stderr and exit 1.
3. Start a server, as today but with a socket:
   - Bind the HTTP listener on `127.0.0.1:0` **and** the `UnixListener` at the
     socket path *before* forking, so both are ready before the child's loops
     start.
   - Record the socket's inode right after binding.
   - Fork. The parent opens `<url>#line=N` (unless `--no-open`), reports the
     URL, and exits. The child does `setsid()`, redirects stdio to `/dev/null`,
     and serves.
   - If the socket directory can't be used safely (see
     [Socket path](#socket-path)), print a warning to `stderr` and run a
     standalone server with no socket. This is today's behavior.

The `#line=N` fragment covers the case where no tab exists to receive a scroll
event yet. The client reads it on first load (see [Client](#client-assetsappjs)).

### Sync mode

1. If the path doesn't end in `.md` or `.markdown` (case-insensitive), exit 0
   **before touching the socket**. This keeps `C-s` in non-Markdown buffers
   down to one process start.
2. If canonicalization fails, exit 0.
3. `send_open(socket, path, Some(line))` with a **500 ms** timeout. Every
   outcome (no socket, refused, timeout, `err`, `ok`) exits 0 with no output.
   Sync never starts a server, never opens a tab, and never removes a stale
   socket (the next open-mode run does that).

### Non-Unix

Open mode behaves as it does today (foreground server, no socket). `--sync`
exits 0 straight away.

## Control channel (`src/control.rs`, new, `#[cfg(unix)]`)

### Socket path

`socket_path(xdg_runtime_dir: Option<&OsStr>, temp_dir: &Path, uid: u32) -> PathBuf`
is a pure function so tests don't have to change the environment:

- If `XDG_RUNTIME_DIR` is set, non-empty and absolute:
  `$XDG_RUNTIME_DIR/mdpreview.sock`.
- Otherwise: `<temp_dir>/mdpreview-<uid>/mdpreview.sock`.

`ensure_socket_dir` creates the socket's directory with mode `0700` if it is
missing. If it already exists, it checks that the directory is a real directory
(not a symlink), is owned by `uid`, and has no group or other permission bits.
This applies to `$XDG_RUNTIME_DIR` too. Both modes run the check *before
connecting*, not only before binding. Otherwise a socket planted by another
user could learn our paths and hand back a URL for us to open. If the check
fails, open mode falls back to a standalone server and sync mode exits 0.

### Protocol

One request and one reply per connection. Both are UTF-8, tab-separated, and
end with a newline:

```
request:  open\t<absolute path>\t<line or empty>\n
reply:    ok\t<url>\t<active SSE clients>\n
          err\t<reason>\n
```

- The client rejects paths containing `\t` or `\n` before connecting. Open mode
  reports this as an error; sync mode exits 0.
- `format_request` / `parse_request` and `format_reply` / `parse_reply` are
  pure functions and are unit tested.
- Both sides set 1 s read and write timeouts. The client may use a shorter one
  (500 ms in sync mode).

### Server side

`control::spawn_listener(listener: UnixListener, handle: impl Fn(Request) -> Reply)`
accepts connections **one at a time** on a single thread. Requests are tiny
and handling one is just a mutex update and a channel send. A client that sends
nothing is dropped after the 1 s read timeout. A malformed request gets
`err\tbad request`.

## Server (`src/server.rs`)

### Events

`()` is replaced by an event type on every channel:

```rust
pub enum Event { Reload, Scroll(u32) }
```

`watch.rs` sends `Event::Reload`. The dispatcher fans each event out to every
client's `Sender<Event>`, as it does today with `()`.

The SSE stream uses named events:

```
event: reload
data:

event: scroll
data: 42

: ping
```

### Current document

`State.file: PathBuf` becomes:

```rust
current: Arc<Mutex<Current>>,
events_tx: Sender<Event>, // cloned into each new watcher

struct Current {
    path: PathBuf,
    _watcher: Option<RecommendedWatcher>,
}
```

Handling `open(path, line)`:

1. Check that `path` is absolute and is an existing file. If not, reply
   `err\t<reason>`.
2. If `path` differs from `current.path`, start a new watcher on it
   (`watch_file(&path, events_tx.clone())`). If that fails, use `None` and say
   nothing. Then replace `Current`, which drops the old watcher, and send
   `Event::Reload`.
3. If a line is given, send `Event::Scroll(line)`.
4. Reply `ok\t<url>\t<active>`.

`/content` clones `current.path` while holding the lock, then reads and renders
it without the lock. The response carries an `X-Mdpreview-File: <file name>`
header, which the client uses for `document.title`.

### Lifetime

The idle rule is unchanged: exit 15 s after the last SSE client disconnects,
counting only once the first client has connected. Before exiting, the monitor
removes the socket **only if the inode at the path still matches the recorded
one**. If two open-mode runs race and both start servers, the loser can't
delete the winner's socket. The loser keeps serving its own tab until the tab
closes.

To make this testable, `serve()` takes a config:

```rust
pub struct Config {
    pub url: String,                    // reported in `ok` replies
    pub idle_grace: Option<Duration>,   // None disables the exit monitor
    #[cfg(unix)]
    pub control: Option<ControlSocket>, // None for a standalone server
}

pub struct ControlSocket {
    pub listener: UnixListener,
    pub path: PathBuf,
    pub inode: u64, // recorded in main right after bind, before fork
}
```

Production passes `Some(15 s)`. Tests pass `None`, so the monitor never calls
`process::exit` in the middle of a test run.

## Render (`src/render.rs`)

- Enable `options.render.sourcepos = true`. Block elements gain
  `data-sourcepos="L:C-L:C"`, with line numbers counted from the top of the
  file, front matter included. Raw HTML blocks don't get one. The client falls
  back to the nearest block before them.
- The front matter `<details>` gets a `data-sourcepos` built from the
  `FrontMatter` node's own sourcepos, so a cursor inside the front matter
  scrolls to the top block.
- The exact-equality test `documents_without_front_matter_are_unchanged` is
  updated to the new attribute-bearing output.

## Client (`assets/app.js`, `assets/app.css`)

- Listen with `addEventListener("reload", …)` and
  `addEventListener("scroll", …)` instead of `onmessage`.
- `collectMermaidBlocks` copies `data-sourcepos` from the original `<pre>` onto
  the new `<pre class="mermaid">`.
- **`findBlock(line)`:** look at block-level elements with `data-sourcepos`,
  ignoring any inline elements comrak marks.
  1. Pick the element with the smallest range that contains `line`.
  2. Otherwise, pick the last element that starts before `line` (for example
     the cursor is on a blank line or inside raw HTML).
  3. Otherwise, pick the top of the document.
- **Scroll + highlight:**
  `el.scrollIntoView({ block: "center", behavior: "smooth" })`, then add
  `.mdpreview-target`. That class is a CSS outline which fades out over about
  1.2 s, and it is removed on `animationend`.
- **Rendering changes the layout.** Mermaid diagrams change height once they
  render, so a scroll made before rendering finishes can land in the wrong
  place. The client keeps `pendingScroll = { line, at }`:
  - The `scroll` event stores `pendingScroll`. If no load is in progress, it
    scrolls and highlights straight away.
  - `loadContent` restores the previous `scrollY` as it does today, *unless*
    `pendingScroll` was requested less than 1.5 s before this load **started**
    (or while it ran). In that case, after `await mermaid.run`, it scrolls to
    the line again and highlights the new element. The reload replaced the
    old element, so any earlier highlight was cut short. This covers a save,
    which sends `scroll` right away and `reload` about 80 ms later once the
    watcher fires. It also covers a file switch, where the target block exists
    only after the reload, and a slow first render (all of `mermaid.md`),
    which is why freshness is measured from when the load starts, not when it
    ends.
- **First load:** if `location.hash` matches `#line=N`, seed `pendingScroll`
  from it, then clear the hash with `history.replaceState` so reloading the
  page doesn't jump back.
- **Title:** set `document.title` to `<X-Mdpreview-File> — mdpreview`.

## Helix bindings

After this feature lands, `~/.config/helix/config.toml` changes to:

```toml
[keys.normal]
"C-s" = [":w", ':sh mdpreview --sync --line %{cursor_line} "%{buffer_name}"']

[keys.insert]
"C-s" = ["normal_mode", ":w", ':sh mdpreview --sync --line %{cursor_line} "%{buffer_name}"']

[keys.normal."\\"]
m = { command = ':sh mdpreview --line %{cursor_line} "%{buffer_name}"', label = "Markdown preview" }
```

## Error handling

The rule: **open mode (`\ m`) reports problems, sync mode (`C-s`) never does.**

| Situation | Open (`\ m`) | Sync (`C-s`) |
|---|---|---|
| File doesn't exist | stderr, exit 1 | exit 0 |
| Not a `.md`/`.markdown` file | preview it anyway | exit 0 before touching the socket |
| No socket or connection refused | remove the stale socket, start a server | exit 0 |
| No reply before the timeout | stderr after 1 s, exit 1 | exit 0 after 500 ms |
| Reply `err\t<reason>` | print the reason, exit 1 | exit 0 |
| Socket dir unsafe (owner/mode) | warn, standalone server | exit 0 |
| Path contains tab/newline | stderr, exit 1 | exit 0 |
| Watcher fails on switch | silent. The page still loads, but won't live-reload | same |
| Current file deleted or unreadable | the existing inline error in the page | same |

## Testing

All tests are inline `#[cfg(test)]` modules, following the existing
convention.

- **`control.rs` unit tests:**
  - Request and reply round-trips: with and without a line, and paths with
    spaces.
  - Paths containing a tab or newline are rejected.
  - Malformed requests and replies are rejected.
  - `socket_path` with XDG set, unset, empty and relative.
- **`main.rs` unit tests:** `parse_args` for both modes, `--no-open`, a
  missing file, a non-numeric `--line`, and unknown flags.
- **`render.rs` unit tests:**
  - Blocks carry `data-sourcepos`.
  - The front matter `<details>` covers its lines.
  - The exact-equality test is updated.
- **Integration test (in-process, no fork, no browser):**
  - Setup: start `serve()` on a temporary socket and a `127.0.0.1:0` listener
    with `idle_grace: None`, then attach a raw `/events` reader.
  - `open A 3` returns `ok` and produces `event: scroll` / `data: 3`.
  - `open B` produces `event: reload`, and `/content` then serves B with
    `X-Mdpreview-File: B.md`.
  - Writing to B on disk produces `event: reload`, which shows the watcher
    moved.
  - A malformed request gets an `err` reply.
- **Manual checklist in Helix**, using the `examples/` fixtures:
  1. `\ m` opens a tab scrolled to the cursor, with the highlight.
  2. `C-s` in another `.md` file switches the tab and updates its title.
  3. `C-s` in a `.rs` buffer does nothing and returns in under 50 ms (timed).
  4. `C-s` below a mermaid diagram lands on the right block once it renders.
  5. Closing the tab makes the server exit and remove its socket.
  6. After `kill -9` of the server, the next `\ m` recovers from the stale
     socket.

  Steps 3, 5 and 6 can be run from a shell with `--no-open`.

## Known limitations

- Pressing `\ m` twice before the browser has connected opens two tabs, because
  the server still reports 0 clients the second time. Both tabs then follow
  the server. This is acceptable, and not worth tracking "tab launching" state
  for.
- There is one current document per server. Every open tab shows it.

## Docs to update

- `README.md`: new flags, Helix bindings, and the auto-save recommendation.
- `CLAUDE.md`: the `control.rs` module, the socket and protocol, sourcepos,
  `Event`, and `serve()`'s `Config`.
