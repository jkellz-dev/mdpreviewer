# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`mdpreview <file.md>` is a single-binary Rust CLI (edition 2024) that serves a live-reloading browser preview of one Markdown file, with mermaid diagrams.

**Purpose:** give Helix the "edit Markdown, see it rendered live in a browser" workflow that nvim gets from plugins like markdown-preview.nvim. Helix has no plugin system, so the preview has to be an external process launched from a keybinding (`:sh mdpreview "%{file_path_absolute}"`). Helix stays the only editor, and the browser just follows file saves. Several design constraints follow from this:

- **Never block the editor.** `:sh` waits for the command to finish and captures its output, so the process has to detach and close its stdio right away (see `main.rs`).
- **Stay quiet.** Any stdout would show up in a Helix popup, so the URL is printed only when stdout is a TTY.
- **Reload only when the file is written.** Helix has no hook for unsaved buffer changes, so the preview updates when the file is saved, not while you type. That's why reloads are driven by a filesystem watcher.
- **Clean up after itself.** No editor process owns the server, so it exits once the browser tab is gone.

## Commands

Tasks are defined in `.config/mise/config.toml`:

```sh
mise run build          # cargo build --release
mise run install        # build + symlink target/release/mdpreview into ~/.local/bin
mise run update-vendor  # re-download assets/vendor/* (MERMAID_VERSION / GH_MD_CSS_VERSION override)
```

Standard `cargo build`, `cargo clippy`, `cargo fmt`, `cargo test` apply. Tests are inline `#[cfg(test)]` modules (currently only `render.rs`). Run one with `cargo test <name_substring>`.

To try a change manually: `cargo run -- path/to/file.md`. On Unix the process forks and the parent exits immediately, so the server runs detached in the background. It prints the URL only when stdout is a TTY, and exits on its own ~15s after the last browser tab closes. Kill stray servers with `pkill mdpreview`.

## Architecture

Request/reload flow across the four modules:

1. **`main.rs`** binds `127.0.0.1:0` *before* forking, so the browser opened by the parent can connect through the kernel accept backlog before the child's server loop starts. The child calls `setsid()` and redirects stdio to `/dev/null`, so the launching editor's pipe sees EOF. On non-Unix platforms it runs in the foreground.
2. **`watch.rs`** watches the file's *parent directory*, not the file itself, so editors that save via atomic rename don't leave a stale inode watch. It filters events down to the target path, ignores `Access` events, debounces bursts (80ms), and sends `()` on an mpsc channel.
3. **`server.rs`** (tiny_http, one thread per request):
   - A dispatcher thread fans each reload signal out to per-client `Sender`s, one per open `/events` SSE connection.
   - `/events` bypasses tiny_http's buffered chunked writer (`request.into_writer()`) and writes raw SSE with a flush after every event. A 10s heartbeat lets it detect dead sockets.
   - A monitor thread exits the process once the active SSE count (tracked by the `ActiveGuard` RAII type) has been 0 for 15s. It only arms after the first client has connected.
   - `/content` re-reads and re-renders the file on every request. There is no caching.
4. **`render.rs`** uses comrak with GFM extensions and `render.unsafe = true`, so raw HTML passes through; this is intentional for a local-only preview. Mermaid fences stay as `<pre><code class="language-mermaid">`. `---` front matter is parsed via comrak's `front_matter_delimiter`. comrak outputs nothing for that node, so `render.rs` prepends it as a collapsed `<details class="frontmatter">` YAML block.

**Client (`assets/app.js`)**: on load and on every SSE message, it fetches `/content`, swaps it into `#content`, converts `code.language-mermaid` blocks to `<pre class="mermaid">`, calls `mermaid.run`, and restores the scroll position. Mermaid rendering happens entirely in the browser.

**Assets are embedded at compile time** (`include_str!` / `include_bytes!` in `server.rs`). Adding a new asset requires both a `const` and a route in `handle()`. `assets/vendor/` holds pinned third-party files (mermaid 11.4.1, github-markdown-css dark 5.8.1). Update them with the script rather than editing by hand.

## Version control

The repo is a colocated Jujutsu (`.jj`) + git repo. Git will usually show a detached `HEAD`, which is normal.
