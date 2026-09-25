# mdpreview

A small, self-contained CLI that serves a live-reloading browser preview of a
Markdown file, including [mermaid](https://mermaid.js.org/) diagrams. Built to
replace an editor plugin (for example nvim's markdown-preview) with an external
process, so it works with any editor that can run a shell command, such as
Helix.

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

[keys.normal."\\".m]
label = "Markdown"
m = { command = ':sh mdpreview --line %{cursor_line} "%{buffer_name}"', label = "Preview (start or switch)" }
r = { command = ':sh mdpreview --sync --line %{cursor_line} "%{buffer_name}"', label = "Scroll preview to cursor" }
```

`%{buffer_name}` is relative to Helix's working directory, which is also where
`:sh` runs. Helix has no absolute-path variable.

## Building and installing

This repo uses [mise](https://mise.jdx.dev/) for tasks:

```sh
mise run build          # cargo build --release
mise run install        # build + symlink target/release/mdpreview into ~/.local/bin
mise run update-vendor  # refresh the vendored browser assets
```

Or with cargo directly:

```sh
cargo build --release
```

## Vendored assets

The browser assets are vendored under `assets/vendor/` so the tool works
offline and pins known-good versions:

- `mermaid.min.js` (mermaid, default 11.17.2)
- `github-markdown.css` (github-markdown-css dark, default 5.8.1)

Refresh them with `mise run update-vendor` (or `sh scripts/update-vendor.sh`).
Override versions via `MERMAID_VERSION` / `GH_MD_CSS_VERSION`.

## How it works

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
