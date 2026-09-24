# mdpreview

A small, self-contained CLI that serves a live-reloading browser preview of a
Markdown file, including [mermaid](https://mermaid.js.org/) diagrams. Built to
replace an editor plugin (for example nvim's markdown-preview) with an external
process, so it works with any editor that can run a shell command, such as
Helix.

## Usage

```sh
mdpreview path/to/file.md
```

It binds a random local port, opens your default browser at that URL, and
renders the file. Saving the file reloads the content in place (scroll position
preserved). On Unix the process detaches from its launcher, so it can be run
from an editor command without blocking. When the last browser tab closes, the
server shuts itself down after a short grace period.

### Helix

Bind it to a key in `config.toml`, passing the current buffer's absolute path:

```toml
[keys.normal."\\"]
m = ":sh mdpreview \"%{file_path_absolute}\""
```

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
  (GFM extensions; raw HTML passed through for fidelity). Fenced `mermaid`
  blocks are left as code blocks and turned into diagrams by the client.
- `watch.rs` watches the file's parent directory (to survive editor
  atomic-rename saves) and emits debounced reload signals.
- `server.rs` serves the shell page, the rendered fragment (`/content`), an SSE
  reload stream (`/events`), and the embedded assets.
- `main.rs` binds the port before forking, opens the browser in the parent, and
  runs the server in the detached child.
