# Typst Rendering

Date: 2026-10-01
Status: design approved in chat; awaiting spec review

## Goal

Preview Typst (`.typ`) documents the same way Markdown is previewed today: the Helix preview keybinding opens a live
browser preview, saves reload it, `C-s` scrolls it to the cursor line, and a single shared server switches between
documents and exits once no tab is open. One server previews either kind; switching between a `.md` and a `.typ`
buffer behaves exactly like switching between two `.md` buffers.

### Decisions Made in Chat

- **Paged output.** Each page is rendered as SVG and the pages are stacked vertically, so the preview matches the PDF
  (fonts, page size, margins, math). Typst's HTML export was rejected as too incomplete.
- **Embed the compiler** (`typst`, `typst-svg`, `typst-kit`, `typst-ide` as needed) rather than shelling out to the
  `typst` CLI. A long-lived `World` gives incremental recompiles, source-to-layout mapping for cursor sync, and the set
  of files a compile read.
- **Track the current typst release (0.15.x)**, and set `rust-version` to the latest stable Rust, 1.99, rather than the
  oldest compiler the dependencies allow (1.92). The toolchain pinned in mise moves to 1.99.0 with it, since Cargo
  refuses to build a crate that declares a newer `rust-version` than the running compiler.
- **Package downloads on.** `@preview/...` imports resolve through `typst-kit`'s package store, using the shared
  `~/.cache/typst/packages` cache and downloading on first use.
- **rustls, not OpenSSL.** `typst-kit` is used with `system-packages` but without `system-downloader` (the feature
  that pulls in `native-tls` and `openssl`). Downloads go through a small `Downloader` implementation of our own on
  ureq 3 (rustls with the `ring` provider, gzip), so no C TLS library is linked on any platform.

### Non-Goals

- Typst HTML export, or recolouring pages for the dark theme.
- Cursor sync inside `#include`d files (they still trigger reloads).
- Browser → editor sync (clicking a page to move the cursor).
- Following links inside Typst output.
- A loading indicator for slow first compiles.
- An opt-out cargo feature for Typst support.

## Architecture

### Document Kinds

`render::is_markdown` becomes `render::kind(path) -> Option<Kind>`, with `Kind::Markdown` (`.md`, `.markdown`) and
`Kind::Typst` (`.typ`), compared case-insensitively. Every gate that currently requires Markdown accepts either kind:
the CLI modes in `main.rs` (open, sync, restart, with the refusal message reworded) and `POST /open` in `server.rs`. A
relative link from a Markdown document to a `.typ` file is therefore followed. `followLink` in `app.js` intercepts
`.typ` links alongside `.md`/`.markdown`.

### `src/typeset.rs`

A new module, named to avoid clashing with the `typst` crate. It owns:

- **A `World` implementation**: the main file's id, a source and file cache keyed by `FileId` and invalidated by file
  fingerprint (so an unchanged import is not re-parsed), the library, the font book, today's date, and package
  resolution. It records every file read during a compile, as canonical paths.
- **Fonts**: `typst-kit`'s system scan plus its embedded fonts, loaded once per process into a `OnceLock` and shared by
  every session. Tests use the embedded fonts only, so results do not depend on the machine.
- **Packages**: `typst-kit`'s `SystemPackages` (shared cache directory, Typst Universe index and tarballs), given an
  `HttpsDownloader`: an implementation of `typst_kit::downloader::Downloader` on a `ureq::Agent`. It honours proxy
  environment variables (`Proxy::try_from_env`), verifies certificates against the OS trust store (ureq's
  `platform-verifier` feature, so a corporate CA works as it does for the typst CLI), and maps HTTP 404 to
  `io::ErrorKind::NotFound`, as the trait requires.
- **`Session`**: the `World` for one main file, plus the result of the last successful compile. Its compile entry point
  returns the HTML fragment for `/content` and the set of dependency paths.

### Server Integration

- `Current` gains `typst: Option<typeset::Session>`. It is created when a `.typ` file becomes current and dropped with
  the rest of `Current` on a switch, the same way the watchers are.
- Compiles go through the session's mutex, so tabs reloading at the same time do not compile in parallel. The compile
  happens outside the `current` lock, following the existing pattern of cloning what is needed and releasing it.
- `/content` dispatches on `render::kind`. Markdown is unchanged. For Typst it compiles and returns the fragment
  described below, with the same `Content-Type` and `X-Mdpreviewer-File` headers.
- **Dependency watching** reuses the image mechanism. After each compile, the dependency set (minus the main file,
  which is already watched) is compared with the watched set. When it changes, it is re-watched with
  `watch::watch_files(…, Event::Reload, …)`. Editing an included chapter, an imported module, an image or a data file
  sends `reload`.

### Content Fragment

```html
<div class="typst-errors" data-sourcepos="L:1-L:1">…</div>   <!-- only on failure -->
<details class="typst-warnings">…</details>                  <!-- only with warnings -->
<div class="typst-page" data-sourcepos="A:1-B:1">
  <svg …>…</svg>
  <div class="typst-line" data-sourcepos="17:1-17:1" style="top:31.2%;height:2.1%"></div>
  …
</div>
…
```

- One `typst-page` per page, holding `typst_svg::svg(page)` and positioned line markers. The page's `data-sourcepos`
  spans the first to the last main-file line with output on it, so `refreshZoom` can find the page again after a
  reload.
- **Line markers**: walk each page's frame tree, accumulating group transforms, and resolve each text glyph's span to a
  1-based line in the **main file**. Spans in other files and detached spans are skipped. For each line, record the
  minimum and maximum y of its glyphs on that page and emit one `typst-line` marker with `top` and `height` as
  percentages of the page height, so markers stay aligned at any rendered width. A line that spans two pages gets a
  marker on each page; `findBlock` breaks ties in favour of the later element, so a scroll lands on the second.
- **Errors**: when a compile fails, the fragment is an error banner followed by the **last successful pages**, or just
  the banner if there has never been a successful compile. The banner lists each diagnostic as
  `file:line:col: message`, with its hints, HTML-escaped. It carries the `data-sourcepos` of the first error located in
  the main file, if any.
- **Warnings** appear in a collapsed `<details class="typst-warnings">` above the pages and never replace them.
- A read error on the main file is reported inline, the same way as for Markdown. A failed package download is an
  ordinary compile error.

## Client (`assets/app.js`, `assets/app.css`)

No new scroll path is needed. `findBlock` already selects elements by `data-sourcepos`, so the `typst-line` markers make
`scrollToLine`, `#line=N`, the pending-scroll replay after a reload, and the `.mdpreviewer-target` flash all work
unchanged. The flash outlines the marker's strip of the page.

- `BLOCK_SELECTOR` is a tag list, so it gains `div.typst-line[data-sourcepos]` and
  `div.typst-errors[data-sourcepos]` explicitly. `.typst-page` is deliberately **not** a scroll target. Its range
  contains every line on the page, so a line without a marker (a `#set` rule, a blank line) would match the whole page
  instead of falling back to the nearest earlier marker.
- `ZOOM_SELECTOR` gains `.typst-page svg`. `zoomKey` finds the enclosing `.typst-page` by `data-sourcepos`.
- `followLink` treats `.typ` as a document link.
- CSS: `.typst-page` is a white sheet with a shadow, centred with a gap between pages, `position: relative` for the
  markers. Its SVG is `width: 100%; height: auto`. `.typst-line` is `position: absolute; left: 0; right: 0` with no
  background. The error banner and warnings get styling that fits the dark theme.
- Mermaid conversion and relative-image rewriting find nothing in Typst output and need no change.

## Testing

Inline `#[cfg(test)]` modules, following the repo's conventions.

- **`render::kind`**: `.typ`, `.TYP`, `.md`, `.markdown`, `.rs`, no extension.
- **`typeset`**:
  - A multi-page document produces that many `typst-page` divs.
  - A syntax error produces the banner with the right line in `data-sourcepos`.
  - An error after a success keeps the previous pages under the banner.
  - The marker for a known line lands on the expected page, and its `top` is ordered correctly relative to an earlier
    line's.
  - An `#include`d file appears in the dependency set.
  - Warnings render without replacing pages.
- **`server.rs` integration** (real server, temp socket, `idle_grace: None`):
  - `/content` for a `.typ` file returns pages.
  - Editing an included file sends `reload`.
  - `POST /open` accepts a `.typ` link.
- **`main.rs`**: its gate lives in `main()`, which no unit test reaches; `render::kind`'s tests cover what it decides.
- **Fixture**: `examples/typst.typ` (lorem ipsum, several pages, headings, math, a table, a figure with an image, and an
  `#include` of a second file) for manual `cargo run -- examples/typst.typ` checks.

## Docs and Release

- `Cargo.toml`: new dependencies, `rust-version = "1.99"` with a comment saying it tracks the latest stable Rust and
  the toolchain pinned in `.config/mise/config.toml`, and Typst added to the description and keywords.
- `.config/mise/config.toml`: `rust` pinned to `1.99.0`.
- CI: the MSRV job checks toolchain `1.99`, the new `rust-version`. The release builds need no new system libraries; confirm with
  `cargo tree -i openssl-sys` and `cargo tree -i native-tls` both finding nothing.
- README, the usage string and CLAUDE.md (overview, architecture module list, examples) describe Typst support.
