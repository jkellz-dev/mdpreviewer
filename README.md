# mdpreviewer

A small, self-contained CLI that serves a live-reloading browser preview of a
Markdown file, including [mermaid](https://mermaid.js.org/) diagrams. Built to
replace an editor plugin (for example nvim's markdown-preview) with an external
process, so it works with any editor that can run a shell command, such as
Helix.

## Usage

```sh
mdpreviewer [--line N] [--no-open] path/to/file.md   # open a preview (or reuse one)
mdpreviewer --sync [--line N] path/to/file.md        # update a running preview only
mdpreviewer --restart [--line N] path/to/file.md     # stop any running server, then open
mdpreviewer --quit                                   # stop the running server
```

The first run binds a random local port, opens your default browser at that
URL, and renders the file. Saving the file reloads the content in place. On
Unix the server detaches from its launcher, so it can be run from an editor
command without blocking, and it shuts itself down shortly after the last
browser tab closes.

Only one preview server runs per user. Later runs hand their file to it over a
Unix socket (`$XDG_RUNTIME_DIR/mdpreviewer.sock`) instead of starting another, so
the open tab switches to the new file. A new tab opens only if none is open.

- `--line N` scrolls the preview to source line `N`, centering and briefly
  highlighting that block.
- `--sync` only talks to a running server. It never starts one, never opens a
  tab and never prints anything, so it is cheap enough to run on every save.
- Every mode that takes a file ignores anything that is not `.md` or
  `.markdown`, so a preview binding is inert in a source buffer. A leading `~`
  in the path is expanded.
- `--quit` stops the running server and its browser tabs stop updating. It takes
  no file, and exits 0 whether or not a server was running.
- `--restart` is `--quit` followed by a normal open. Browser assets are compiled
  into the binary, so after a rebuild this is how you get a running server to
  serve the new ones.
- `--no-open` prints the URL instead of opening a browser.

### Zooming a diagram or image

Click a mermaid diagram, an image, a table or a code block to blow it up to
fill the window. In the overlay:

| Input                              | Effect                                                     |
| ---------------------------------- | ---------------------------------------------------------- |
| scroll wheel                       | zoom in or out around the pointer                          |
| drag                               | pan                                                        |
| `0`                                | fit to the window again                                    |
| `+` / `-`                          | zoom from the centre                                       |
| double-click                       | toggle between fitting the window and the document's scale |
| `f`                                | true browser fullscreen                                    |
| `esc`, the `×`, or a click outside | close                                                      |

Links are left alone, so a linked image follows its link, and clicking after
selecting text does not zoom. The overlay follows live reloads: saving the
file re-renders what it is showing and keeps your zoom, and it closes if the
block is gone.

### Helix

Helix has no plugin system, so the preview is an ordinary command bound to a
key. `:sh` runs in Helix's working directory and `%{buffer_name}` is relative to
it (Helix has no absolute-path variable), so the two line up.

```toml
[editor]
# Save shortly after edits, so the preview follows typing, not just C-s.
auto-save = { focus-lost = true, after-delay.enable = true, after-delay.timeout = 300 }

[keys.normal]
# Save, then scroll a running preview to the cursor. A silent no-op otherwise.
"C-s" = [":w", ':sh mdpreviewer --sync --line %{cursor_line} "%{buffer_name}"']

[keys.insert]
"C-s" = ["normal_mode", ":w", ':sh mdpreviewer --sync --line %{cursor_line} "%{buffer_name}"']

# A "Markdown" submenu. This assumes `\` is your leader key; any free key works.
[keys.normal.\\.m]
label = "Markdown"
m = { command = [":w", ':sh mdpreviewer --line %{cursor_line} "%{buffer_name}"'], label = "Preview (save, start or switch)" }
r = { command = [":w", ':sh mdpreviewer --restart --line %{cursor_line} "%{buffer_name}"'], label = "Restart preview server" }
q = { command = ":sh mdpreviewer --quit", label = "Quit preview server" }
```

| Key   | Does                                                               |
| ----- | ------------------------------------------------------------------ |
| `C-s` | Save, then scroll the preview to the cursor. Runs on every save.   |
| `\mm` | Save, then start the preview or point the running one at this file |
| `\mr` | Save, then restart the server and open this file                   |
| `\mq` | Stop the server                                                    |

Helix has no per-filetype keymaps, so these run in every buffer. `mdpreviewer`
only acts on `.md` and `.markdown` files; anywhere else it refuses with
`not a Markdown file: <name>`, which Helix shows in its shell popup. That also
covers `[scratch]` buffers, where `:w` fails first but Helix runs the rest of
the list anyway.

Success is silent. `mdpreviewer` prints the URL only when stdout is a terminal,
so a binding does not pop one up on every keypress, but failures always print.

Helix reports a file outside its working directory as `~/...`, and the binding
quotes it so the shell cannot expand it. `mdpreviewer` expands a leading `~`
itself.

Bindings like these need `mdpreviewer` on the `PATH` Helix inherits;
`mise run install` symlinks it into `~/.local/bin`.

Use `\mr` after rebuilding. The browser assets are compiled into the binary, so
a server started from an older build keeps serving the assets it was built with,
no matter how many times you press `\mm`. `--restart` replaces the process, so
it picks up both new assets and new server code. It binds a new port, so any tab
from the previous server stops updating; close it.

## Building and installing

Install the published binary with cargo:

```sh
cargo install mdpreviewer
```

Prebuilt macOS and Linux binaries are attached to each
[release](https://github.com/jkellz-dev/mdpreviewer/releases).

To build from a clone, this repo uses [mise](https://mise.jdx.dev/) for tasks:

```sh
mise run build          # cargo build --release
mise run install        # build + symlink target/release/mdpreviewer into ~/.local/bin
mise run update-vendor  # refresh the vendored browser assets
```

Or with cargo directly:

```sh
cargo build --release
```

## Linting and formatting

[hk](https://hk.jdx.dev/) drives every linter and formatter, so CI and a local
run do the same work:

```sh
mise run lint  # hk check --all
mise run fix   # hk fix --all
```

`hk.pkl` lists the steps: rustfmt and clippy for Rust, oxfmt for the browser
JavaScript, yamlfmt and actionlint and zizmor for the workflows, taplo for TOML,
and rumdl for Markdown. `examples/` is excluded from Markdown formatting because
those files are fixtures whose odd formatting is the point, and
`assets/vendor/` is excluded everywhere because it is pinned upstream code.

hk can install git hooks with `hk install`, but this repo is developed with
Jujutsu, which does not run git hooks, so run the tasks directly.

## Releasing

[release-plz](https://release-plz.dev/) runs on every push to `main`. It keeps a
pull request open that bumps the version and writes `CHANGELOG.md` from the
Conventional Commit messages. Merging that PR publishes the crate to crates.io,
tags the commit, creates the GitHub release, and attaches macOS arm64 and Linux
x86_64 binaries.

Publishing needs a `CARGO_REGISTRY_TOKEN` repository secret holding a crates.io
token with the `publish-new` and `publish-update` scopes.

Before pushing, rehearse locally:

```sh
mise run release:check           # lint, test, and package the crate
mise run release:preview         # show the version bump and changelog
mise run release:publish-dry-run # rehearse the publish, changing nothing
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
- `assets/app.js` renders the fragment, turns mermaid fences into diagrams,
  reloads on SSE events, scrolls to the cursor line, and provides the
  click-to-zoom overlay.
- `control.rs` is the control socket: its location, a one-line protocol, the
  client used by later runs, and the server-side listener.
- `main.rs` parses the CLI, reuses a running server when there is one, and
  otherwise binds the port and socket before forking and runs the server in
  the detached child.
