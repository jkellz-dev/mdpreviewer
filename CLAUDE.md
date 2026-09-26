# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`mdpreviewer <file.md>` is a single-binary Rust CLI (edition 2024) that serves a live-reloading browser preview of one
Markdown file, with mermaid diagrams.

**Purpose:** give Helix the "edit Markdown, see it rendered live in a browser" workflow that nvim gets from plugins like
markdown-preview.nvim. Helix has no plugin system, so the preview has to be an external process launched from a
keybinding (`:sh mdpreviewer --line %{cursor_line} "%{buffer_name}"`; `C-s` also runs `mdpreviewer --sync …` to scroll a
running preview). Helix stays the only editor, and the browser just follows file saves. Several design constraints
follow from this:

- **Never block the editor.** `:sh` waits for the command to finish and captures its output, so the process has to
  detach and close its stdio right away (see `main.rs`).
- **Stay quiet.** Any stdout would show up in a Helix popup, so the URL is printed only when stdout is a TTY.
- **Reload only when the file is written.** Helix has no hook for unsaved buffer changes, so the preview updates when
  the file is saved (Helix's auto-save makes that frequent). That's why reloads are driven by a filesystem watcher.
- **One server, driven by later invocations.** Helix can only run commands, so later `mdpreviewer` runs pass the file
  and cursor line to the running server over a Unix socket. `--sync` mode (bound to `C-s`) must stay silent and fast
  because it runs on every save of every file.
- **Clean up after itself.** No editor process owns the server, so it exits once the browser tab is gone.

## Commands

Tasks are defined in `.config/mise/config.toml`:

```sh
mise run build          # cargo build --release
mise run install        # build + symlink target/release/mdpreviewer into ~/.local/bin
mise run update-vendor  # re-download assets/vendor/* (MERMAID_VERSION / GH_MD_CSS_VERSION override)
mise run lint           # hk check --all: every linter and formatter
mise run fix            # hk fix --all
mise run release:check  # lint, test, and package the crate, as CI would
```

Linting and formatting go through [hk](https://hk.jdx.dev/) (`hk.pkl`), not ad-hoc tool invocations: rustfmt, clippy,
oxfmt for `assets/app.js`, yamlfmt, actionlint and zizmor for the workflows, taplo for TOML, rumdl for Markdown.
`assets/vendor/` is excluded everywhere and `examples/` and `docs/superpowers/` are excluded from Markdown rules. Every
tool is pinned in `.config/mise/config.toml`, and CI runs the same `mise run lint`. Git hooks are pointless here because
the repo is developed with Jujutsu, which does not run them.

Releases are automated with [release-plz](https://release-plz.dev/): a push to `main` keeps a version-bump PR open, and
merging it publishes to crates.io, tags, creates the GitHub release and attaches macOS arm64 and Linux x86_64 binaries.
The binary job hangs off the release job rather than a `release: published` trigger, because a release created with
`GITHUB_TOKEN` does not start new workflow runs. Actions are pinned to commit SHAs, with Dependabot bumping them on a 7
day cooldown.

Standard `cargo build`, `cargo clippy`, `cargo fmt`, `cargo test` apply. Tests are inline `#[cfg(test)]` modules.
`server.rs` has in-process integration tests (real server, temp socket, `idle_grace: None`). `src/testutil.rs` has
`TestDir`, which picks a base directory short enough for the control sockets bound inside it: a Unix socket path is
capped at 104 bytes, and macOS's `TMPDIR` under `/var/folders` overflows that, so it falls back to `/tmp`. Run one with
`cargo test <name_substring>`.

To try a change manually: `cargo run -- examples/<file>.md`. `examples/` holds fixed lorem ipsum fixtures: `basics.md`
(GFM, front matter, raw HTML, and two data-URI SVG images (one plain, one linked, to cover the zoom overlay's link
guard)), `mermaid.md` (one of each diagram type plus a deliberately invalid one) and `front-matter.md` (escaping and
edge cases). On Unix the process forks and the parent exits immediately, so the server runs detached in the background.
It prints the URL only when stdout is a TTY, and exits on its own ~15s after the last browser tab closes. Use
`--no-open` to get the URL without opening a tab, and a scratch `XDG_RUNTIME_DIR` to avoid reusing your real server.
Kill test servers by PID (`ss -xlpn | grep mdpreviewer.sock` shows it).

## Architecture

Request/reload flow across the five modules:

1. **`main.rs`** parses `[--line N] [--no-open] [--sync|--quit|--restart] [<file>]` (the mode flags are mutually
   exclusive; `--quit` takes no file). Open mode first asks a running server over the control socket to switch to the
   file. If one answers, it exits, opening a tab only when no client is connected. Otherwise it binds the control socket
   and `127.0.0.1:0` _before_ forking, so the browser opened by the parent (at `<url>#line=N`) can connect through the
   kernel accept backlog before the child's server loop starts. The child calls `setsid()` and redirects stdio to
   `/dev/null`, so the launching editor's pipe sees EOF. Sync mode only sends the request (500 ms timeout) and always
   exits 0 silently. Any mode that takes a file expands a leading `~` (Helix reports files outside its cwd as `~/...`
   and the quoted binding stops the shell expanding it) and refuses non-Markdown files, open and restart included, so a
   preview keybinding pressed in a source buffer cannot leave a dead server. Refusals go through `fail` (stderr, exit 1)
   rather than `report`, which is TTY-gated: Helix's `:sh` pops up whatever a command writes, so a silent refusal reads
   as a broken binding, while a TTY-gated success keeps the URL out of that popup. `--quit` sends `quit` and exits 0
   whether or not a server answered; `--restart` does that, polls `control::is_listening` (20 ms steps, 2 s cap) until
   the old server has released the socket, then opens normally. On non-Unix platforms it runs in the foreground and
   `--sync`, `--quit` and `--restart` are no-ops.
2. **`control.rs`** (Unix) handles the socket:
   - Its path: `$XDG_RUNTIME_DIR/mdpreviewer.sock`, or a 0700 `mdpreviewer-<uid>` dir in the temp dir.
   - The one-line protocol: `open\t<path>\t<line>\n` → `ok\t<url>\t<clients>\n`, `quit\n` → `bye\n`, or
     `err\t<reason>\n` for anything malformed. `bye` is written _before_ the server is told to go, so the client sees a
     reply rather than a closed connection.
   - `send`, which tells `NoServer` apart from `Timeout`, and `is_listening`, which `--restart` polls.
   - A single-threaded listener with a 1 s per-connection timeout.
   - `remove_socket_if_ours`, which removes the socket only if its inode matches the one recorded at bind, so a racing
     server's socket survives.
3. **`watch.rs`** watches the file's _parent directory_, not the file itself, so editors that save via atomic rename
   don't leave a stale inode watch. It filters events down to the target path, ignores `Access` events, debounces bursts
   (80ms), and sends `Event::Reload` on an mpsc channel.
4. **`server.rs`** (tiny_http, one thread per request):
   - `serve(server, file, Config { url, idle_grace, control, exit_on_quit })` owns the event channel and the current
     document: a `Current { path, watcher }` behind a mutex. `spawn_listener` also takes an `on_quit` callback: with
     `Config::exit_on_quit` (off in tests) it removes the socket and exits the process. A control `open` for a different
     file replaces it (dropping the old watch) and sends `Event::Reload`. A line sends `Event::Scroll(n)`.
   - A dispatcher thread fans each `Event` out to per-client `Sender`s, one per open `/events` SSE connection, as named
     events (`event: reload` / `event: scroll` + `data: <line>`).
   - `/events` bypasses tiny_http's buffered chunked writer (`request.into_writer()`) and writes raw SSE with a flush
     after every event. A 10s heartbeat lets it detect dead sockets.
   - A monitor thread exits the process once the active SSE count (tracked by the `ActiveGuard` RAII type) has been 0
     for `idle_grace` (15s), removing the socket first. It only arms after the first client has connected.
     `idle_grace: None` disables it for tests.
   - `/content` re-reads and re-renders the current file on every request, with no caching, and names it in
     `X-Mdpreviewer-File` (percent-encoded).
5. **`render.rs`** uses comrak with GFM extensions and `render.unsafe = true`, so raw HTML passes through; this is
   intentional for a local-only preview. Mermaid fences stay as `<pre><code class="language-mermaid">`. `---` front
   matter is parsed via comrak's `front_matter_delimiter`. comrak outputs nothing for that node, so `render.rs` prepends
   it as a collapsed `<details class="frontmatter">` YAML block. `render.sourcepos` is on, so blocks carry
   `data-sourcepos="L:C-L:C"` (inline elements too); the front matter `<details>` gets its node's range.

**Client (`assets/app.js`)**: on load and on every `reload` event, it:

- Fetches `/content`, swaps it into `#content` and sets the title from `X-Mdpreviewer-File`.
- Converts `code.language-mermaid` blocks to `<pre class="mermaid">`, keeping `data-sourcepos`, and awaits
  `mermaid.run`.
- Then either restores the scroll position or re-applies a recent scroll request.

On `scroll` events and `#line=N`, `findBlock` picks the innermost block-level element whose sourcepos contains the line
(else the last one before it), centers it and flashes a `.mdpreviewer-target` outline. A scroll requested up to 1.5s
before a load started is re-applied after that load renders, because a save sends `scroll` at once and `reload` ~80ms
later. Mermaid rendering happens entirely in the browser.

**Zoom overlay (`assets/app.js`, styles in `assets/app.css`)**: a delegated `click` on `#content` (so it survives the
`innerHTML` swap) matches `ZOOM_SELECTOR` (`pre.mermaid svg, img, table, pre:not(.mermaid)`) and opens a full-window
overlay holding a _clone_ of that element at its intrinsic size: `naturalWidth` for images, `scrollWidth`/`scrollHeight`
for code blocks and tables that scroll sideways, the rendered box for an SVG. Pan and zoom are one
`translate(-50%, -50%) translate(tx, ty) scale(s)` on the figure, which is centred with `left/top: 50%`; wheel zoom
solves for `tx`/`ty` so the point under the cursor stays put. Clicks are ignored inside an `<a>`, while text is
selected, and with a modifier. Clones set `draggable = false` and the overlay swallows `dragstart`, because a native
image drag fires `pointercancel` and would abort a pan mid-drag. `loadContent` calls `refreshZoom()`, which finds the
same block in the new document by `data-sourcepos` and re-clones it (keeping the current zoom and pan), or closes the
overlay if that block is gone, since otherwise the overlay would show a stale render.

**Assets are embedded at compile time** (`include_str!` / `include_bytes!` in `server.rs`). Adding a new asset requires
both a `const` and a route in `handle()`. A running server therefore keeps serving the assets it was built with, and
later `mdpreviewer` runs hand their file to it over the control socket rather than starting a new one, so after changing
anything under `assets/` the old server has to be killed (`lsof -a -p <pid> -iTCP -sTCP:LISTEN`, or
`ss -xlpn | grep mdpreviewer.sock` on Linux) before the change shows up. `assets/vendor/` holds pinned third-party files
(mermaid 11.17.2, github-markdown-css dark 5.8.1). Update them with the script rather than editing by hand.

@CLAUDE.local.md
