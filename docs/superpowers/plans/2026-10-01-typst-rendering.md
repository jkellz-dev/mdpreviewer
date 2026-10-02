# Typst Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preview `.typ` documents in mdpreviewer the way `.md` documents are previewed: paged SVG output, live reload
on save (including saves of included files), `C-s` cursor sync, and the same single shared server.

**Architecture:** A new `typeset` module embeds the Typst compiler behind a long-lived `Session` (a `World` built from
typst-kit's `FileStore`, `FontStore` and `SystemPackages`). It renders each page as an SVG with invisible,
`data-sourcepos`-tagged line markers on top, so the existing client scroll code works unchanged. The server keeps one
`Session` per current Typst document inside `Current`, dispatches `/content` on the document's kind, and watches the
files each compile read. Package downloads go through a small ureq/rustls `Downloader` in a new `https` module.

**Tech Stack:** Rust 2024, typst / typst-layout / typst-svg / typst-kit 0.15.1, ureq 3 (rustls + ring,
platform-verifier), tiny_http, notify, vanilla JS/CSS client.

**Spec:** `docs/superpowers/specs/2026-10-01-typst-rendering-design.md`

## Global Constraints

- typst crates pinned to `0.15.1`: `typst`, `typst-layout`, `typst-svg`, `typst-kit`.
- `typst-kit` features: `scan-fonts`, `embedded-fonts`, `system-files`, `datetime`. Never `system-downloader`.
- `ureq = { version = "3.4.2", default-features = false, features = ["rustls", "gzip", "platform-verifier"] }`.
- `rust-version = "1.99"` (latest stable as of 2026-10-01); CI's `msrv` job uses toolchain `"1.99"`; mise pins
  `rust = "1.99.0"`. Cargo refuses to build when the compiler is older than `rust-version`, so these move together.
- No OpenSSL and no native-tls: `cargo tree -i openssl-sys` and `cargo tree -i native-tls` must both fail with "did not
  match any packages".
- Typst documents are recognised by the `.typ` extension, case-insensitively. Markdown stays `.md` / `.markdown`.
- Version control is Jujutsu (`jj`), not git. Commit a task with `jj commit -m "<message>"`. No `Co-Authored-By` or
  other attribution trailers in messages.
- Comments: no counts or enumerations that adding code would falsify (see `~/.claude/CLAUDE.md`). Match the
  surrounding comment density and voice.
- Lint gate for every task: `cargo clippy --all-targets --quiet -- -D warnings` and `cargo fmt --check` are clean;
  `mise run lint` before the final commit of tasks that touch JS, CSS, Markdown, YAML or TOML.
- Tests are inline `#[cfg(test)]` modules. Anything using `crate::testutil::TestDir` is `#[cfg(unix)]`-gated, as
  `testutil` is.

## Review Focus

1. **A save that breaks the document mid-edit** (Helix auto-save writes half-typed Typst constantly). Expected: the
   previous pages stay visible under an error banner. Pinned by `a_failed_compile_keeps_the_last_pages` (Task 2).
2. **An `#include` of a file in a directory that does not exist yet.** Expected: the include is a compile error, and
   edits to the document's other included files still reload the preview. A single unwatchable path must not cost the
   whole dependency watch. Pinned by `a_dependency_in_a_missing_directory_does_not_stop_the_watch` (Task 5).
3. **The document deleted, or replaced by a binary file, while previewed.** Expected: an error banner, never a panic
   that kills the request thread or poisons the session. Pinned by `a_deleted_document_is_an_error_not_a_panic` and
   `a_document_that_is_not_utf8_is_an_error` (Task 2).
4. **Compiler messages containing HTML** (`#panic("<b>")`, file names with `&`). Expected: shown as text, not
   injected. Pinned by `diagnostics_are_escaped` (Task 2).
5. **Switching from a Typst document to a Markdown one and back.** Expected: each renders with its own renderer, and
   the Typst session starts fresh. Pinned by `switching_between_markdown_and_typst_renders_each` (Task 4).

---

## File Structure

| File | Change | Responsibility |
| --- | --- | --- |
| `Cargo.toml`, `Cargo.lock` | modify | typst + ureq dependencies, `rust-version = "1.99"`, description and keywords |
| `.config/mise/config.toml` | modify | pin `rust` to `1.99.0` |
| `.github/workflows/ci.yml` | modify | MSRV job toolchain `1.99` |
| `src/https.rs` | create | `HttpsDownloader`: typst-kit `Downloader` on ureq/rustls |
| `src/typeset.rs` | create | `Session` / `Rendered`: compile a Typst file to the `/content` fragment, plus dependencies |
| `src/render.rs` | modify | `Kind` + `kind()` replace `is_markdown()` |
| `src/server.rs` | modify | `Current.typst` session, `/content` dispatch, `/open` accepts Typst, dependency watch |
| `src/main.rs` | modify | `mod https; mod typeset;`, gates accept Typst, usage text |
| `assets/app.js` | modify | scroll targets, zoom selector, `.typ` link following, refusal text |
| `assets/app.css` | modify | page sheets, invisible markers, error banner, warnings |
| `examples/typst.typ`, `examples/typst-chapter.typ`, `examples/typst-figure.svg` | create | manual-test fixture |
| `README.md`, `CLAUDE.md` | modify | document Typst support |

---

### Task 1: Dependencies, MSRV and the rustls package downloader

**Files:**

- Modify: `Cargo.toml`, `Cargo.lock`, `.config/mise/config.toml:5`, `.github/workflows/ci.yml:50-53`, `src/main.rs:20-25`
- Create: `src/https.rs`

**Interfaces:**

- Produces: `crate::https::HttpsDownloader` — `impl Default` (builds the agent) and
  `impl typst_kit::downloader::Downloader` (`stream(&self, key: &dyn Any, url: &str) -> io::Result<(Option<usize>,
  Box<dyn Read>)>`, HTTP 404 → `io::ErrorKind::NotFound`).

- [ ] **Step 1: Move to the latest Rust, add the dependencies and raise the MSRV**

In `.config/mise/config.toml`, change the `rust` pin from `"1.98.1"` to `"1.99.0"` (keep the `components` list), then
install it:

```bash
mise install
rustc --version   # expect 1.99.0
```

Then add the dependencies:

```bash
cargo add typst@0.15.1 typst-layout@0.15.1 typst-svg@0.15.1
cargo add typst-kit@0.15.1 --features scan-fonts,embedded-fonts,system-files,datetime
cargo add ureq@3.4.2 --no-default-features --features rustls,gzip,platform-verifier
```

Then edit `Cargo.toml`. Replace the `rust-version` comment and value:

```toml
# The latest stable Rust, matching the toolchain pinned in
# .config/mise/config.toml. Bump the two together.
rust-version = "1.99"
```

In `.github/workflows/ci.yml`, in the `msrv` job, change `toolchain: "1.88"` to `toolchain: "1.99"` (the comment above
it, "Keep in step with `rust-version` in Cargo.toml.", stays).

- [ ] **Step 2: Confirm no OpenSSL**

Run: `cargo tree -i openssl-sys; cargo tree -i native-tls`
Expected: both print `error: package ID specification ... did not match any packages`.

- [ ] **Step 3: Write the failing tests**

Create `src/https.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read};
    use std::thread;

    use tiny_http::{Response, Server};
    use typst_kit::downloader::Downloader;

    use super::HttpsDownloader;

    /// A plain-HTTP server answering `/ok` with a body and anything else
    /// with a 404. The status mapping does not depend on TLS.
    fn serve() -> String {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        thread::spawn(move || {
            for request in server.incoming_requests() {
                let response = if request.url() == "/ok" {
                    Response::from_string("package bytes")
                } else {
                    Response::from_string("missing").with_status_code(404)
                };
                let _ = request.respond(response);
            }
        });
        url
    }

    #[test]
    fn a_download_streams_the_body_with_its_size() {
        let url = serve();
        let (size, mut reader) = HttpsDownloader::default()
            .stream(&(), &format!("{url}/ok"))
            .unwrap();
        let mut body = String::new();
        reader.read_to_string(&mut body).unwrap();
        assert_eq!(body, "package bytes");
        assert_eq!(size, Some(body.len()));
    }

    #[test]
    fn a_404_is_not_found() {
        let url = serve();
        let err = HttpsDownloader::default()
            .stream(&(), &format!("{url}/gone"))
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn other_failures_are_not_not_found() {
        // Nothing listens on port 1.
        let err = HttpsDownloader::default()
            .stream(&(), "http://127.0.0.1:1/x")
            .err()
            .unwrap();
        assert_ne!(err.kind(), ErrorKind::NotFound);
    }
}
```

Register the module in `src/main.rs`, after `mod control;` in alphabetical order. Until Task 4 wires it into the
server, only tests use it, so silence dead-code warnings for the non-test build:

```rust
mod control;
#[cfg_attr(not(test), allow(dead_code))]
mod https;
mod render;
```

- [ ] **Step 4: Run them to verify they fail**

Run: `cargo test https`
Expected: compile error, `cannot find type HttpsDownloader in super`.

- [ ] **Step 5: Implement the downloader**

Put this above the test module in `src/https.rs`:

```rust
//! The HTTPS client Typst uses to download `@preview` packages.
//!
//! typst-kit's own downloader links OpenSSL through `native-tls`. This one is
//! ureq on rustls, so no C TLS library is linked on any platform.

use std::any::Any;
use std::io::{self, Read};

use typst_kit::downloader::Downloader;
use ureq::tls::{RootCerts, TlsConfig};

/// Fetches package archives and the package index. Certificates are checked
/// against the operating system's trust store, as the typst CLI does, so a
/// CA installed for a corporate proxy works. ureq reads the proxy from the
/// usual environment variables.
pub struct HttpsDownloader(ureq::Agent);

impl Default for HttpsDownloader {
    fn default() -> Self {
        let tls = TlsConfig::builder()
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .user_agent(concat!("mdpreviewer/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        HttpsDownloader(agent)
    }
}

impl Downloader for HttpsDownloader {
    fn stream(&self, _key: &dyn Any, url: &str) -> io::Result<(Option<usize>, Box<dyn Read>)> {
        let response = self.0.get(url).call().map_err(|err| match err {
            // The trait asks for `NotFound` here: typst-kit reports it as
            // "package not found" rather than as a network failure.
            ureq::Error::StatusCode(404) => io::Error::new(io::ErrorKind::NotFound, err),
            err => io::Error::other(err),
        })?;
        let body = response.into_body();
        let size = body
            .content_length()
            .and_then(|len| usize::try_from(len).ok());
        Ok((size, Box::new(body.into_reader())))
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test https`
Expected: `a_download_streams_the_body_with_its_size`, `a_404_is_not_found`, `other_failures_are_not_not_found` pass.

- [ ] **Step 7: Lint and commit**

Run: `cargo clippy --all-targets --quiet -- -D warnings && cargo fmt --check && mise run lint`
Expected: clean. The code in this plan was verified with clippy 1.98.1; if 1.99 adds a lint that fires, fix what it
reports rather than allowing it.

```bash
jj commit -m "build: add the typst compiler and a rustls package downloader

Move to Rust 1.99, the latest stable, as both the pinned toolchain and
rust-version."
```

---

### Task 2: Compile a Typst document to pages

**Files:**

- Create: `src/typeset.rs`
- Modify: `src/main.rs` (module list)

**Interfaces:**

- Consumes: `crate::https::HttpsDownloader::default()` (Task 1).
- Produces:
  - `pub struct typeset::Session`, `pub fn Session::new(path: &Path) -> Session` (`path` absolute),
    `pub fn Session::render(&mut self) -> Rendered`.
  - `pub struct typeset::Rendered { pub html: String, pub dependencies: Vec<PathBuf> }`. `dependencies` is sorted,
    canonical project files the compile read, excluding the document itself and package files.
  - Fragment classes the client relies on: `div.typst-page` (one per page, containing the page `<svg>`),
    `div.typst-errors` (with `data-sourcepos="L:1-L:1"` of the first error in the document, when there is one),
    `details.typst-warnings`, `span.typst-hint`.

- [ ] **Step 1: Write the failing tests**

Create `src/typeset.rs` with only the test module:

```rust
#[cfg(all(test, unix))]
mod tests {
    use std::fs;

    use super::{Rendered, Session};
    use crate::testutil::TestDir;

    /// A scratch directory holding `main.typ`, and a session on it.
    struct Doc {
        dir: TestDir,
        session: Session,
    }

    impl Doc {
        fn new(name: &str, source: &str) -> Doc {
            let dir = TestDir::new(name);
            fs::write(dir.join("main.typ"), source).unwrap();
            let session = Session::new(&dir.join("main.typ"));
            Doc { dir, session }
        }

        fn write(&self, name: &str, contents: &str) {
            fs::write(self.dir.join(name), contents).unwrap();
        }

        fn render(&mut self) -> Rendered {
            self.session.render()
        }
    }

    /// The pages of a fragment, each from just after `<div class="typst-page"`
    /// to the next page.
    fn pages(html: &str) -> Vec<&str> {
        html.split("<div class=\"typst-page\"").skip(1).collect()
    }

    const TWO_PAGES: &str = "#set page(height: 10cm)\nfirst\n\nsecond\n#pagebreak()\nthird\n";

    #[test]
    fn each_page_is_an_svg() {
        let mut doc = Doc::new("typst-pages", TWO_PAGES);
        let html = doc.render().html;
        let pages = pages(&html);
        assert_eq!(pages.len(), 2, "{html}");
        assert!(pages.iter().all(|page| page.contains("<svg")), "{html}");
        assert!(!html.contains("typst-errors"), "{html}");
    }

    #[test]
    fn a_recompile_reads_the_edited_file() {
        let mut doc = Doc::new("typst-edit", "one\n");
        assert_eq!(pages(&doc.render().html).len(), 1);
        doc.write("main.typ", "one\n#pagebreak()\ntwo\n");
        assert_eq!(pages(&doc.render().html).len(), 2);
    }

    #[test]
    fn included_files_are_dependencies() {
        let mut doc = Doc::new("typst-include", "intro\n#include \"chapter.typ\"\n");
        doc.write("chapter.typ", "chapter text\n");
        let rendered = doc.render();
        assert_eq!(rendered.dependencies, vec![doc.dir.join("chapter.typ")]);
    }

    #[test]
    fn a_failed_first_compile_shows_only_the_errors() {
        let mut doc = Doc::new("typst-error", "= Title\n\n#let x = (\n");
        let html = doc.render().html;
        assert!(
            html.starts_with("<div class=\"typst-errors\" data-sourcepos=\"3:1-3:1\">"),
            "{html}"
        );
        assert!(html.contains("<code>main.typ:3:"), "{html}");
        assert!(pages(&html).is_empty(), "{html}");
    }

    #[test]
    fn a_failed_compile_keeps_the_last_pages() {
        let mut doc = Doc::new("typst-stale", "fine\n");
        let good = doc.render().html;
        doc.write("main.typ", "fine\n#let x = (\n");
        let html = doc.render().html;
        assert!(html.contains("typst-errors"), "{html}");
        assert!(html.contains("last version that compiled"), "{html}");
        assert!(html.ends_with(&good), "{html}");
    }

    #[test]
    fn diagnostics_are_escaped() {
        let mut doc = Doc::new("typst-escape", "#panic(\"<b>&\")\n");
        let html = doc.render().html;
        assert!(html.contains("&lt;b&gt;&amp;"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
    }

    #[test]
    fn warnings_do_not_replace_the_pages() {
        let mut doc = Doc::new("typst-warning", "#set text(font: \"No Such Font\")\ntext\n");
        let html = doc.render().html;
        assert!(
            html.contains("<details class=\"typst-warnings\"><summary>1 warning</summary>"),
            "{html}"
        );
        assert!(html.contains("no such font"), "{html}");
        assert_eq!(pages(&html).len(), 1, "{html}");
    }

    #[test]
    fn a_deleted_document_is_an_error_not_a_panic() {
        let mut doc = Doc::new("typst-deleted", "text\n");
        fs::remove_file(doc.dir.join("main.typ")).unwrap();
        let html = doc.render().html;
        assert!(html.contains("typst-errors"), "{html}");
    }

    #[test]
    fn a_document_that_is_not_utf8_is_an_error() {
        let mut doc = Doc::new("typst-binary", "");
        fs::write(doc.dir.join("main.typ"), [0xff, 0xfe, 0x00]).unwrap();
        let html = doc.render().html;
        assert!(html.contains("typst-errors"), "{html}");
    }

    #[test]
    fn files_outside_the_document_directory_are_refused() {
        let mut doc = Doc::new("typst-root", "#include \"../outside.typ\"\n");
        let html = doc.render().html;
        assert!(html.contains("typst-errors"), "{html}");
        assert!(doc.render().dependencies.is_empty());
    }
}
```

Register it in `src/main.rs`, after `mod server;`, with the same temporary attribute as `https`:

```rust
mod server;
#[cfg(all(test, unix))]
mod testutil;
#[cfg_attr(not(test), allow(dead_code))]
mod typeset;
mod watch;
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test typeset`
Expected: compile error, `unresolved imports super::Rendered, super::Session`.

- [ ] **Step 3: Implement the module**

Put this above the test module in `src/typeset.rs`:

```rust
//! Typst to HTML: each page as an SVG.
//!
//! A [`Session`] keeps the compiler's world between renders, so a recompile
//! after a save only redoes the work the edit invalidated.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use comrak::html::escape;
use typst::diag::{FileResult, SourceDiagnostic, Warned};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{DiagSpan, FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World, WorldExt};
use typst_kit::datetime::Time;
use typst_kit::files::{FileStore, FsRoot, SystemFiles};
use typst_kit::fonts::FontStore;
use typst_kit::packages::SystemPackages;
use typst_layout::PagedDocument;

use crate::https::HttpsDownloader;

/// One Typst document being previewed.
pub struct Session {
    world: DocumentWorld,
    /// The pages of the last successful compile, shown under the errors of a
    /// failed one so that saving a half-typed edit does not blank the page.
    last_pages: Option<String>,
}

/// The output of [`Session::render`].
pub struct Rendered {
    /// The fragment `/content` serves.
    pub html: String,
    /// Every project file the compile read other than the document itself:
    /// includes, imports, images, data. Package files are left out; they do
    /// not change under the user.
    pub dependencies: Vec<PathBuf>,
}

impl Session {
    /// Start a session for the document at `path`, which must be absolute.
    /// As with the typst CLI, the document's directory is the project root,
    /// which bounds what it may read.
    pub fn new(path: &Path) -> Session {
        let root = path.parent().expect("an absolute file path has a parent");
        let vpath =
            VirtualPath::virtualize(root, path).expect("a file is inside its own directory");
        let packages = SystemPackages::new(HttpsDownloader::default());
        Session {
            world: DocumentWorld {
                library: LazyHash::new(Library::default()),
                main: RootedPath::new(VirtualRoot::Project, vpath).intern(),
                files: FileStore::new(SystemFiles::new(FsRoot::new(root.to_owned()), packages)),
                time: Time::system(),
            },
            last_pages: None,
        }
    }

    /// Compile the document from disk and render it.
    pub fn render(&mut self) -> Rendered {
        // Mark every cached file stale, so each is read again, and reset the
        // clock, so `datetime.today()` is today's.
        self.world.files.reset();
        self.world.time.reset();
        let Warned { output, warnings } = typst::compile::<PagedDocument>(&self.world);

        let mut html = String::new();
        match output {
            Ok(document) => {
                self.last_pages = Some(render_pages(&document));
            }
            Err(errors) => {
                render_errors(&self.world, &errors, self.last_pages.is_some(), &mut html)
            }
        }
        render_warnings(&self.world, &warnings, &mut html);
        if let Some(pages) = &self.last_pages {
            html.push_str(pages);
        }
        Rendered {
            html,
            dependencies: self.dependencies(),
        }
    }

    fn dependencies(&mut self) -> Vec<PathBuf> {
        let main = self.world.main;
        let (files, ids) = self.world.files.dependencies();
        let mut paths: Vec<PathBuf> = ids
            .filter(|&id| id != main && *id.root() == VirtualRoot::Project)
            .filter_map(|id| files.resolve(id).ok())
            .collect();
        paths.sort();
        paths
    }
}

/// Fonts are found once per process: scanning the system takes a few hundred
/// milliseconds, and the result does not depend on the document.
fn fonts() -> &'static FontStore {
    static FONTS: OnceLock<FontStore> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = FontStore::new();
        // Tests see only the embedded fonts, so layout does not depend on
        // what the machine has installed.
        if !cfg!(test) {
            fonts.extend(typst_kit::fonts::system());
        }
        fonts.extend(typst_kit::fonts::embedded());
        fonts
    })
}

/// Everything the compiler reads goes through here.
struct DocumentWorld {
    library: LazyHash<Library>,
    main: FileId,
    files: FileStore<SystemFiles>,
    time: Time,
}

impl World for DocumentWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        fonts().book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        self.files.source(id)
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.files.file(id)
    }

    fn font(&self, index: usize) -> Option<Font> {
        fonts().font(index)
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        self.time.today(offset)
    }
}

/// Every page as a `typst-page` element holding its SVG.
fn render_pages(document: &PagedDocument) -> String {
    let mut html = String::new();
    for page in document.pages() {
        html.push_str("<div class=\"typst-page\">");
        html.push_str(&typst_svg::svg(page, &Default::default()));
        html.push_str("</div>\n");
    }
    html
}

/// The banner for a failed compile. It carries the line of the first error
/// in the document itself, so a scroll to that line lands on it.
fn render_errors(
    world: &DocumentWorld,
    errors: &[SourceDiagnostic],
    stale: bool,
    html: &mut String,
) {
    html.push_str("<div class=\"typst-errors\"");
    let first_line = errors
        .iter()
        .find_map(|error| match locate(world, error.span) {
            Some((id, line, _)) if id == world.main => Some(line),
            _ => None,
        });
    if let Some(line) = first_line {
        let _ = write!(html, " data-sourcepos=\"{line}:1-{line}:1\"");
    }
    html.push_str("><p>");
    html.push_str(if stale {
        "Typst could not compile the document. Showing the last version that compiled."
    } else {
        "Typst could not compile the document."
    });
    html.push_str("</p>");
    render_diagnostics(world, errors, html);
    html.push_str("</div>\n");
}

fn render_warnings(world: &DocumentWorld, warnings: &[SourceDiagnostic], html: &mut String) {
    if warnings.is_empty() {
        return;
    }
    let plural = if warnings.len() == 1 { "" } else { "s" };
    let _ = write!(
        html,
        "<details class=\"typst-warnings\"><summary>{} warning{plural}</summary>",
        warnings.len()
    );
    render_diagnostics(world, warnings, html);
    html.push_str("</details>\n");
}

/// A list of diagnostics, each as `file:line:col: message` plus its hints.
fn render_diagnostics(world: &DocumentWorld, diagnostics: &[SourceDiagnostic], html: &mut String) {
    html.push_str("<ul>");
    for diagnostic in diagnostics {
        html.push_str("<li>");
        if let Some(id) = diagnostic.span.id() {
            html.push_str("<code>");
            let mut place = display_path(id);
            if let Some((_, line, column)) = locate(world, diagnostic.span) {
                let _ = write!(place, ":{line}:{column}");
            }
            push_escaped(html, &place);
            html.push_str("</code> ");
        }
        push_escaped(html, &diagnostic.message);
        for hint in &diagnostic.hints {
            html.push_str("<br><span class=\"typst-hint\">hint: ");
            push_escaped(html, &hint.v);
            html.push_str("</span>");
        }
        html.push_str("</li>");
    }
    html.push_str("</ul>");
}

/// The file, 1-based line and 1-based column a diagnostic points at.
fn locate(world: &DocumentWorld, span: DiagSpan) -> Option<(FileId, usize, usize)> {
    let id = span.id()?;
    let start = world.range(span)?.start;
    let (line, column) = world.source(id).ok()?.lines().byte_to_line_column(start)?;
    Some((id, line + 1, column + 1))
}

/// A file as the user would name it: relative to the document's directory,
/// or within its package.
fn display_path(id: FileId) -> String {
    let path = id.vpath().get_without_slash();
    match id.root() {
        VirtualRoot::Package(package) => format!("{package}/{path}"),
        VirtualRoot::Project => path.to_owned(),
    }
}

fn push_escaped(html: &mut String, text: &str) {
    escape(html, text).expect("writing to a String cannot fail");
}
```

Notes for the implementer:

- `FileStore::reset` before each compile is what makes a save show up. Without it the cached file is served forever.
- Package files are excluded from `dependencies` by checking `VirtualRoot::Project`. They live in the package cache and
  do not change under the user.
- The project root is the document's directory, as in the typst CLI. `../outside.typ` is refused by typst-kit itself.
- Fonts are loaded once per process; tests skip the system scan so layout is deterministic.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test typeset`
Expected: all tests in `typeset::tests` pass. The first one takes a moment while the embedded fonts load.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets --quiet -- -D warnings && cargo fmt --check`
Expected: clean.

```bash
jj commit -m "feat(typeset): compile Typst documents to SVG pages

A Session keeps the compiler's world between renders. A failed compile
shows its errors above the last pages that compiled."
```

---

### Task 3: Line markers for cursor sync

**Files:**

- Modify: `src/typeset.rs`

**Interfaces:**

- Consumes: `Session`, `render_pages` (Task 2).
- Produces: inside each `div.typst-page`, after its `<svg>`, one
  `<div class="typst-line" data-sourcepos="L:1-L:1" style="top:T%;height:H%">` per line of the document that put text on
  the page. The page div gains `data-sourcepos="FIRST:1-LAST:1"` when it has any markers. Positions are percentages of
  the page height with three decimals.

- [ ] **Step 1: Write the failing tests**

Add to `typeset::tests`, after `fn pages`:

```rust
    /// The `top` of the marker for `line` in `page`, in percent.
    fn marker_top(page: &str, line: usize) -> Option<f64> {
        let tag =
            format!("class=\"typst-line\" data-sourcepos=\"{line}:1-{line}:1\" style=\"top:");
        let rest = &page[page.find(&tag)? + tag.len()..];
        rest[..rest.find('%')?].parse().ok()
    }
```

And these tests, after `each_page_is_an_svg`:

```rust
    #[test]
    fn markers_place_lines_on_their_page_in_order() {
        let mut doc = Doc::new("typst-markers", TWO_PAGES);
        let html = doc.render().html;
        let pages = pages(&html);
        let first = marker_top(pages[0], 2).expect("line 2 on page 1");
        let second = marker_top(pages[0], 4).expect("line 4 on page 1");
        assert!(first < second, "{first} < {second}");
        assert!(marker_top(pages[1], 6).is_some(), "line 6 on page 2");
        assert!(marker_top(pages[1], 2).is_none(), "line 2 only on page 1");
        // A line that puts no text on the page gets no marker.
        assert!(marker_top(pages[0], 1).is_none(), "{html}");
        // The page is tagged with its range of lines.
        assert!(
            pages[0].starts_with(" data-sourcepos=\"2:1-4:1\">"),
            "{}",
            pages[0]
        );
    }

    #[test]
    fn included_text_gets_no_markers() {
        let mut doc = Doc::new("typst-include-lines", "intro\n#include \"chapter.typ\"\n");
        doc.write("chapter.typ", "chapter text\n");
        let html = doc.render().html;
        let page = pages(&html)[0];
        assert!(marker_top(page, 1).is_some(), "{page}");
        // Line 1 of chapter.typ is not line 1 of main.typ, and line 2 of
        // main.typ only names the include.
        assert!(page.starts_with(" data-sourcepos=\"1:1-1:1\">"), "{page}");
    }
```

Extend `a_recompile_reads_the_edited_file` so it also proves markers follow the edit. Replace its body with:

```rust
        let mut doc = Doc::new("typst-edit", "one\n");
        let html = doc.render().html;
        assert_eq!(pages(&html).len(), 1);
        assert!(marker_top(pages(&html)[0], 3).is_none());
        doc.write("main.typ", "one\n\nthree\n#pagebreak()\ntwo\n");
        let html = doc.render().html;
        assert_eq!(pages(&html).len(), 2);
        assert!(marker_top(pages(&html)[0], 3).is_some());
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test typeset`
Expected: `markers_place_lines_on_their_page_in_order`, `included_text_gets_no_markers` and
`a_recompile_reads_the_edited_file` FAIL (no `typst-line` in the output). The rest pass.

- [ ] **Step 3: Implement the markers**

In `src/typeset.rs`:

1. Replace the module doc's first line with:

```rust
//! Typst to HTML: each page as an SVG, overlaid with invisible markers that
//! tag where each source line landed, so the client's line-based scrolling
//! works as it does for Markdown.
```

2. Imports: add `use std::collections::{BTreeMap, HashMap};` above `use std::fmt::Write as _;`, add
   `use typst::layout::{Abs, Frame, FrameItem, Transform};` above the `typst::syntax` import, add `Span` to the
   `typst::syntax` import (`{DiagSpan, FileId, RootedPath, Source, Span, VirtualPath, VirtualRoot}`), and change
   `use typst_layout::PagedDocument;` to `use typst_layout::{Page, PagedDocument};`.

3. Above `/// One Typst document being previewed.`, add:

```rust
/// How far a glyph reaches above and below its baseline, as a fraction of
/// the font size. Close enough for every common font, and a marker only has
/// to cover its line, not trace it.
const ASCENT: f64 = 0.8;
const DESCENT: f64 = 0.2;

```

4. In `Session::render`, change `render_pages(&document)` to `render_pages(&self.world, &document)`.

5. Replace `render_pages` with:

```rust
/// Every page as a `typst-page` element holding its SVG and line markers.
fn render_pages(world: &DocumentWorld, document: &PagedDocument) -> String {
    let main = world.source(world.main).ok();
    let mut lines_of = HashMap::new();
    let mut html = String::new();
    for page in document.pages() {
        let extents = match &main {
            Some(source) => line_extents(source, page, &mut lines_of),
            None => BTreeMap::new(),
        };
        render_page(page, &extents, &mut html);
    }
    html
}

fn render_page(page: &Page, extents: &BTreeMap<usize, (Abs, Abs)>, html: &mut String) {
    let height = page.frame.height().to_pt();
    html.push_str("<div class=\"typst-page\"");
    // The page's line range lets the client find it again after a reload.
    if let (Some(first), Some(last)) = (extents.keys().next(), extents.keys().next_back()) {
        let _ = write!(html, " data-sourcepos=\"{first}:1-{last}:1\"");
    }
    html.push('>');
    html.push_str(&typst_svg::svg(page, &Default::default()));
    for (line, (top, bottom)) in extents {
        // Percentages of the page height keep the markers in place however
        // wide the page is drawn.
        let top = (top.to_pt() / height * 100.0).clamp(0.0, 100.0);
        let bottom = (bottom.to_pt() / height * 100.0).clamp(0.0, 100.0);
        let _ = write!(
            html,
            "<div class=\"typst-line\" data-sourcepos=\"{line}:1-{line}:1\" \
             style=\"top:{top:.3}%;height:{:.3}%\"></div>",
            bottom - top
        );
    }
    html.push_str("</div>\n");
}

/// For each 1-based line of `source` that put text on `page`, the highest and
/// lowest point of that text. Text from other files (includes, packages) is
/// skipped: the editor's line numbers refer to the document itself.
/// `lines_of` caches span lookups across pages.
fn line_extents(
    source: &Source,
    page: &Page,
    lines_of: &mut HashMap<Span, Option<usize>>,
) -> BTreeMap<usize, (Abs, Abs)> {
    let mut extents = BTreeMap::new();
    let mut add = |span: Span, top: Abs, bottom: Abs| {
        if span.id() != Some(source.id()) {
            return;
        }
        let line = *lines_of.entry(span).or_insert_with(|| {
            let offset = source.find(span)?.offset();
            Some(source.lines().byte_to_line(offset)? + 1)
        });
        if let Some(line) = line {
            let extent = extents.entry(line).or_insert((top, bottom));
            extent.0 = extent.0.min(top);
            extent.1 = extent.1.max(bottom);
        }
    };
    walk_text(&page.frame, Transform::identity(), &mut add);
    extents
}

/// Call `f` with the span and vertical extent of every glyph in `frame`,
/// in page coordinates.
fn walk_text(frame: &Frame, ts: Transform, f: &mut impl FnMut(Span, Abs, Abs)) {
    for (pos, item) in frame.items() {
        match item {
            FrameItem::Group(group) => {
                let ts = ts
                    .pre_concat(Transform::translate(pos.x, pos.y))
                    .pre_concat(group.transform);
                walk_text(&group.frame, ts, f);
            }
            FrameItem::Text(text) => {
                let baseline = pos.transform(ts).y;
                let (top, bottom) = (
                    baseline - text.size * ASCENT,
                    baseline + text.size * DESCENT,
                );
                let mut previous = None;
                for glyph in &text.glyphs {
                    // Neighbouring glyphs usually share a span.
                    let span = glyph.span.0;
                    if previous != Some(span) {
                        f(span, top, bottom);
                        previous = Some(span);
                    }
                }
            }
            _ => {}
        }
    }
}
```

Note: math, `#image` and shapes put no text glyphs on the page, so a line holding only an image gets no marker and a
scroll to it falls back to the nearest earlier marked line. That is the intended degradation (spec, "Lines with no
output").

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test typeset`
Expected: every `typeset::tests` test passes.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets --quiet -- -D warnings && cargo fmt --check`

```bash
jj commit -m "feat(typeset): tag where each source line lands on its page

Invisible markers carry data-sourcepos, so the client's line-based
scrolling works for Typst as it does for Markdown."
```

---

### Task 4: Serve Typst documents

**Files:**

- Modify: `src/render.rs:12-20` (+ its tests), `src/server.rs` (`Current`, `serve_content`, `follow_link`, route docs,
  tests), `src/main.rs` (`USAGE`, gates at :152-160 and :257, module attributes), `assets/app.js` (`followLink`,
  `openFailure`, comments)

**Interfaces:**

- Consumes: `typeset::Session::{new, render}`, `typeset::Rendered` (Tasks 2–3).
- Produces:
  - `pub enum render::Kind { Markdown, Typst }` (`Debug, Clone, Copy, PartialEq, Eq`) and
    `pub fn render::kind(path: &Path) -> Option<Kind>`. `render::is_markdown` is removed.
  - `Current.typst: Option<Arc<Mutex<typeset::Session>>>`, `Some` exactly when the current document is Typst.

- [ ] **Step 1: Write the failing `render::kind` test**

In `src/render.rs` tests, replace the `use super::{is_markdown, render_markdown};` import with
`use super::{Kind, kind, render_markdown};` and replace `only_markdown_files_are_synced` with:

```rust
    #[test]
    fn documents_are_recognised_by_extension() {
        for markdown in ["a.md", "docs/README.MD", "notes.markdown", "/abs/x.Md"] {
            assert_eq!(kind(Path::new(markdown)), Some(Kind::Markdown), "{markdown}");
        }
        for typst in ["thesis.typ", "/abs/Notes.TYP"] {
            assert_eq!(kind(Path::new(typst)), Some(Kind::Typst), "{typst}");
        }
        for other in ["main.rs", "[scratch]", "foo.md.bak", ".md", ".typ", "Makefile", ""] {
            assert_eq!(kind(Path::new(other)), None, "{other}");
        }
    }
```

- [ ] **Step 2: Write the failing server tests**

In `src/server.rs`, in the Unix integration-test module (the one with `fn start`), add after
`a_link_that_cannot_be_followed_is_refused`:

```rust
    #[test]
    fn typst_documents_are_served_as_pages() {
        let dir = TestDir::new("typst-content");
        let doc = dir.join("main.typ");
        fs::write(&doc, "= Hello\n").unwrap();
        let preview = start(&dir, &doc);

        let content = get(&preview, "/content");
        assert!(
            content
                .to_ascii_lowercase()
                .contains("x-mdpreviewer-file: main.typ"),
            "{content}"
        );
        assert!(content.contains("<div class=\"typst-page\""), "{content}");
        assert!(content.contains("data-sourcepos=\"1:1-1:1\""), "{content}");
    }

    #[test]
    fn switching_between_markdown_and_typst_renders_each() {
        let dir = TestDir::new("typst-switch");
        let a = dir.join("a.md");
        let b = dir.join("b.typ");
        fs::write(&a, "# A\n").unwrap();
        fs::write(&b, "= B\n").unwrap();
        let preview = start(&dir, &a);

        assert!(matches!(open(&preview, &b, None), Reply::Ok { .. }));
        assert!(get(&preview, "/content").contains("typst-page"));
        assert!(matches!(open(&preview, &a, None), Reply::Ok { .. }));
        let content = get(&preview, "/content");
        assert!(content.contains(">A</h1>"), "{content}");
        assert!(!content.contains("typst-page"), "{content}");
        // Back again: a fresh session, which compiles from scratch.
        fs::write(&b, "= B changed\n").unwrap();
        assert!(matches!(open(&preview, &b, None), Reply::Ok { .. }));
        assert!(get(&preview, "/content").contains("typst-page"));
    }

    #[test]
    fn a_relative_link_to_a_typst_document_is_followed() {
        let dir = TestDir::new("typst-link");
        let a = dir.join("a.md");
        fs::write(&a, "[thesis](thesis.typ)\n").unwrap();
        fs::write(dir.join("thesis.typ"), "= Thesis\n").unwrap();
        let preview = start(&dir, &a);

        assert_eq!(status(&follow(&preview, "thesis.typ")), "204");
        assert!(get(&preview, "/content").contains("typst-page"));
    }
```

In `a_link_that_cannot_be_followed_is_refused`, the `notes.txt` → `400` assertion stays as is: `.txt` is still neither
kind.

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test render:: && cargo test typst_`
Expected: compile error in `render` tests (`cannot find function kind`).

- [ ] **Step 4: Implement `render::kind`**

In `src/render.rs`, replace `is_markdown` and its doc comment with:

```rust
/// The kinds of document the preview renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Markdown,
    Typst,
}

/// What kind of document `path` names, by extension, or `None` for anything
/// the preview does not render. Every CLI mode that takes a file only acts on
/// these, because `C-s` and the preview bindings run for whatever buffer is
/// open, including source files. The server uses it to pick a renderer and to
/// decide which relative links it will follow.
pub fn kind(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" => Some(Kind::Markdown),
        "typ" => Some(Kind::Typst),
        _ => None,
    }
}
```

Update the module doc's first line from `//! Markdown to HTML rendering.` to
`//! Markdown to HTML rendering, and which files are documents at all.`

- [ ] **Step 5: Hold a session in `Current` and render Typst in `/content`**

In `src/server.rs`:

1. Imports: `use crate::{render, watch};` becomes `use crate::{render, typeset, watch};`, and
   `use std::sync::{Arc, Mutex};` becomes `use std::sync::{Arc, Mutex, PoisonError};`.

2. Route doc at the top of the file: change `GET /content                rendered markdown fragment; the` to
   `GET /content                the rendered document fragment; the`, and in the `POST /open` entry change
   `follow a relative Markdown link` to `follow a relative link to a document`.

3. Replace the `Current` doc comment, struct and `impl` with:

```rust
/// The document being previewed, the watch that reloads it, the images it
/// has shown, and for a Typst document the compiler session.
struct Current {
    path: PathBuf,
    /// Held only to keep the watch alive; replacing it stops the old watch.
    _watcher: Option<RecommendedWatcher>,
    /// Every image `/file` has served for this document, canonical. Replacing
    /// one sends [`Event::Images`], through `image_watcher`.
    images: BTreeSet<PathBuf>,
    image_watcher: Option<RecommendedWatcher>,
    /// Shared so that `/content` compiles without holding the `current` lock.
    /// The session's own lock keeps two tabs from compiling at once.
    typst: Option<Arc<Mutex<typeset::Session>>>,
}

impl Current {
    fn new(path: PathBuf, events_tx: &Sender<Event>) -> Self {
        let typst = (render::kind(&path) == Some(render::Kind::Typst))
            .then(|| Arc::new(Mutex::new(typeset::Session::new(&path))));
        Current {
            _watcher: start_watch(&path, events_tx),
            path,
            images: BTreeSet::new(),
            image_watcher: None,
            typst,
        }
    }
}
```

4. Replace `serve_content` with:

```rust
/// Render the current document, with its file name in `X-Mdpreviewer-File` for
/// the page title. Problems are reported inline so the browser shows them
/// rather than a blank page.
fn serve_content(request: Request, state: &State) {
    // Clone what is needed so the lock is not held while rendering.
    let (path, typst) = {
        let current = state.current.lock().unwrap();
        (current.path.clone(), current.typst.clone())
    };
    let body = match typst {
        // A panic in the compiler poisons the lock. Every render starts by
        // marking the session's files stale, so carry on with it.
        Some(session) => {
            let mut session = session.lock().unwrap_or_else(PoisonError::into_inner);
            session.render().html
        }
        None => match fs::read_to_string(&path) {
            Ok(markdown) => render::render_markdown(&markdown),
            Err(err) => format!(
                "<h1>mdpreviewer</h1><p>Could not read <code>{}</code>: {}</p>",
                path.display(),
                err
            ),
        },
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let response = Response::from_data(body.into_bytes())
        .with_header(header("Content-Type", "text/html; charset=utf-8"))
        .with_header(header("X-Mdpreviewer-File", &percent_encode(&name)));
    let _ = request.respond(response);
}
```

5. In `follow_link`, update the doc comment's first line to
   `/// \`POST /open\`: follow a relative link to another document. The` and replace the kind check:

```rust
    // Checked on the resolved path, so a `.md` symlink to something else is
    // refused rather than rendered.
    if render::kind(&path).is_none() {
        return status(request, 400, "not a Markdown or Typst file");
    }
```

- [ ] **Step 6: Open the CLI gates and wire the modules**

In `src/main.rs`:

1. Remove the `#[cfg_attr(not(test), allow(dead_code))]` line above `mod https;`: the server now reaches it through
   `typeset::Session::new`. Keep the one above `mod typeset;` until Task 5, because nothing outside tests reads
   `Rendered::dependencies` yet.

2. Replace `USAGE`:

```rust
const USAGE: &str = "usage: mdpreviewer [--line N] [--no-open] <file.md|file.typ>\n       \
                     mdpreviewer --sync [--line N] <file.md|file.typ>\n       \
                     mdpreviewer --restart [--line N] [--no-open] <file.md|file.typ>\n       \
                     mdpreviewer --quit";
```

3. The open/restart gate:

```rust
        Mode::Restart | Mode::Open
            if render::kind(Path::new(args.file.as_deref().unwrap_or_default())).is_none() =>
        {
            fail(&format!(
                "not a Markdown or Typst file: {}",
                args.file.as_deref().unwrap_or_default()
            ));
        }
```

4. In `run_sync`: `if render::kind(Path::new(file)).is_none() {`.

Run `grep -rn is_markdown src` and fix anything left; it should find nothing.

- [ ] **Step 7: Follow `.typ` links in the client**

In `assets/app.js`:

1. In `followLink`: `if (!/\.(md|markdown)$/i.test(path)) return;` becomes
   `if (!/\.(md|markdown|typ)$/i.test(path)) return;`.
2. In `openFailure`: `` `Not a Markdown file: ${name}` `` becomes `` `Not a Markdown or Typst file: ${name}` ``, and
   the comment above it ends `...and 400 for one that is neither Markdown nor Typst.`
3. The comments that say "another Markdown file" above `followLink` and "Relative Markdown links are followed" above
   `retargetExternalLinks` become "another document" and "Relative links to documents are followed".

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test`
Expected: everything passes, including `documents_are_recognised_by_extension`, `typst_documents_are_served_as_pages`,
`switching_between_markdown_and_typst_renders_each` and `a_relative_link_to_a_typst_document_is_followed`.

- [ ] **Step 9: Lint and commit**

Run: `cargo clippy --all-targets --quiet -- -D warnings && cargo fmt --check && mise run lint`

```bash
jj commit -m "feat: preview Typst documents

.typ files are accepted wherever Markdown is: the CLI modes, control-socket
switches and followed links. /content renders them through typeset."
```

---

### Task 5: Reload when an included file changes

**Files:**

- Modify: `src/server.rs` (`Current`, `State`, `serve_content`, tests), `src/main.rs` (module attribute)

**Interfaces:**

- Consumes: `typeset::Rendered::dependencies` (Task 2), `watch::watch_files(&[PathBuf], Event, Sender<Event>)`.
- Produces: `State::watch_dependencies(&self, doc: &Path, dependencies: Vec<PathBuf>)`. After every Typst render, the
  watched set equals the compile's dependencies whose directory exists. A change sends `Event::Reload`.

- [ ] **Step 1: Write the failing tests**

In the Unix integration-test module of `src/server.rs`, after `a_relative_link_to_a_typst_document_is_followed`:

```rust
    #[test]
    fn editing_an_included_file_reloads() {
        let dir = TestDir::new("typst-dep");
        let doc = dir.join("main.typ");
        let chapter = dir.join("chapter.typ");
        fs::write(&doc, "#include \"chapter.typ\"\n").unwrap();
        fs::write(&chapter, "one\n").unwrap();
        let preview = start(&dir, &doc);
        let mut events = Events::connect(&preview);

        // Rendering is what learns the dependencies and starts their watch.
        get(&preview, "/content");
        events.settle();

        fs::write(&chapter, "two\n").unwrap();
        assert_eq!(events.next(), "event: reload\ndata:\n\n");
    }

    #[test]
    fn a_dependency_in_a_missing_directory_does_not_stop_the_watch() {
        let dir = TestDir::new("typst-dep-missing");
        let doc = dir.join("main.typ");
        let chapter = dir.join("chapter.typ");
        fs::write(
            &doc,
            "#include \"chapter.typ\"\n#include \"later/draft.typ\"\n",
        )
        .unwrap();
        fs::write(&chapter, "one\n").unwrap();
        let preview = start(&dir, &doc);
        let mut events = Events::connect(&preview);

        assert!(get(&preview, "/content").contains("typst-errors"));
        events.settle();

        fs::write(&chapter, "two\n").unwrap();
        assert_eq!(events.next(), "event: reload\ndata:\n\n");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test included_file dependency_in_a_missing`
Expected: both FAIL with `timed out waiting for an event` (nothing watches `chapter.typ`).

- [ ] **Step 3: Implement the dependency watch**

In `src/main.rs`, remove the `#[cfg_attr(not(test), allow(dead_code))]` line above `mod typeset;`: this step reads
`Rendered::dependencies`, the last part of `typeset` that only tests used.

In `src/server.rs`:

1. Add to `Current`, after the `typst` field:

```rust
    /// The files the last Typst compile read besides the document itself.
    /// Changing one sends [`Event::Reload`], through `dependency_watcher`.
    dependencies: Vec<PathBuf>,
    dependency_watcher: Option<RecommendedWatcher>,
```

and initialise them in `Current::new` with `dependencies: Vec::new(),` and `dependency_watcher: None,`.

2. Add to `impl State`, after `watch_image`:

```rust
    /// Watch the files the last compile of `doc` read, unless the set is
    /// unchanged or `doc` stopped being current in the meantime. A file in a
    /// directory that does not exist (an include not written yet) is left
    /// out, because one unwatchable path would fail the whole watch.
    fn watch_dependencies(&self, doc: &Path, dependencies: Vec<PathBuf>) {
        let mut current = self.current.lock().unwrap();
        if current.path != doc || current.dependencies == dependencies {
            return;
        }
        let watchable: Vec<PathBuf> = dependencies
            .iter()
            .filter(|path| path.parent().is_some_and(Path::is_dir))
            .cloned()
            .collect();
        current.dependency_watcher = if watchable.is_empty() {
            None
        } else {
            match watch::watch_files(&watchable, Event::Reload, self.events_tx.clone()) {
                Ok(watcher) => Some(watcher),
                Err(err) => {
                    eprintln!("mdpreviewer: dependency watch failed: {err}");
                    None
                }
            }
        };
        current.dependencies = dependencies;
    }
```

3. In `serve_content`, replace the Typst arm with:

```rust
        // A panic in the compiler poisons the lock. Every render starts by
        // marking the session's files stale, so carry on with it.
        Some(session) => {
            let rendered = session
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .render();
            state.watch_dependencies(&path, rendered.dependencies);
            rendered.html
        }
```

Note: `dependencies` is compared with the full list (not only the watchable part), so creating `later/` and then
`later/draft.typ` is picked up by the next render, which a save of `main.typ` triggers.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: everything passes.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets --quiet -- -D warnings && cargo fmt --check`

```bash
jj commit -m "feat(server): reload when a file a Typst document reads changes"
```

---

### Task 6: Client scroll targets, zoom and page styling

**Files:**

- Modify: `assets/app.js` (`BLOCK_SELECTOR` at :21-43, `ZOOM_SELECTOR` at :331), `assets/app.css`

**Interfaces:**

- Consumes: the fragment classes from Tasks 2–3 (`typst-page`, `typst-line`, `typst-errors`, `typst-warnings`,
  `typst-hint`).
- Produces: nothing other tasks use.

There are no JS tests in this repo; verification is manual (Step 4).

- [ ] **Step 1: Scroll targets**

In `assets/app.js`, replace the `BLOCK_SELECTOR` comment and definition with:

```js
// Elements that make good scroll targets. comrak also puts data-sourcepos on
// inline elements (em, code, a, ...); those are ignored. A Typst page is not a
// target: its range spans every line on it, so a line with no marker of its
// own would match the whole page instead of the nearest marker before it.
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
  .concat(["div.typst-line[data-sourcepos]", "div.typst-errors[data-sourcepos]"])
  .join(",");
```

- [ ] **Step 2: Zoom a page**

Change `ZOOM_SELECTOR` to:

```js
const ZOOM_SELECTOR = "pre.mermaid svg, .typst-page > svg, img, table, pre:not(.mermaid)";
```

`resolveZoomTarget` uses `closest(ZOOM_SELECTOR)`, so a click anywhere on the page's drawing resolves to its `<svg>`,
and `zoomKey` finds the page's `data-sourcepos`. The markers must not intercept the click (CSS below).

- [ ] **Step 3: Styling**

Append to `assets/app.css`:

```css
/*
 * Typst pages: white sheets on the dark background, as a PDF viewer shows
 * them. The SVG keeps the document's own colours.
 */
.typst-page {
  position: relative;
  margin: 0 auto 1.5rem;
  background: #fff;
  box-shadow: 0 2px 12px rgba(0, 0, 0, 0.6);
}

.typst-page > svg {
  display: block;
  width: 100%;
  height: auto;
  cursor: zoom-in;
}

/* Line markers only give scrolling a position to aim at. Clicks go through
 * to the page, so it can still be zoomed. */
.typst-line {
  position: absolute;
  left: 0;
  right: 0;
  pointer-events: none;
}

.typst-errors {
  margin-bottom: 1.5rem;
  padding: 0.75rem 1rem;
  border: 1px solid #f85149;
  border-radius: 6px;
  background: rgba(248, 81, 73, 0.1);
}

.typst-errors p {
  margin: 0;
}

.typst-errors ul,
.typst-warnings ul {
  margin: 0.5rem 0 0;
}

.typst-warnings {
  margin-bottom: 1.5rem;
  color: #d29922;
}

.typst-hint {
  color: #8b949e;
}
```

- [ ] **Step 4: Check it by hand**

The server embeds assets at build time, so kill any running preview first (`ss -xlpn | grep mdpreviewer.sock` shows
its PID). Create a scratch document, `/tmp/typst-check/main.typ`, with two pages of text (a `#lorem(400)` paragraph on
line 3 is enough), then:

```bash
XDG_RUNTIME_DIR=$(mktemp -d) cargo run -- --line 3 /tmp/typst-check/main.typ
```

Check in the browser:

- The pages show as white sheets, stacked, filling the column width.
- The page scrolls to line 3 and flashes an outline over that paragraph's strip of the page.
- Clicking a page opens the zoom overlay; wheel zoom and drag work; `esc` closes it.
- Breaking the file (`#let x = (` on a new line) and saving shows the red banner above the old pages. Fixing it
  removes the banner.

- [ ] **Step 5: Lint and commit**

Run: `mise run lint`
Expected: clean (oxfmt formats `app.js`).

```bash
jj commit -m "feat(client): scroll to, zoom and style Typst pages"
```

---

### Task 7: Fixture and docs

**Files:**

- Create: `examples/typst.typ`, `examples/typst-chapter.typ`, `examples/typst-figure.svg`
- Modify: `Cargo.toml` (`description`, `keywords`), `README.md`, `CLAUDE.md`

- [ ] **Step 1: The fixture**

`examples/typst-figure.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" width="240" height="120" viewBox="0 0 240 120">
  <rect width="240" height="120" fill="#ddf4ff"/>
  <circle cx="60" cy="60" r="40" fill="#0969da"/>
  <rect x="130" y="25" width="80" height="70" fill="#1a7f37"/>
</svg>
```

`examples/typst-chapter.typ`:

```typst
== Included chapter

This section lives in `typst-chapter.typ`. Editing and saving it reloads the
preview, though `C-s` in its buffer does not scroll.

#lorem(120)
```

`examples/typst.typ`:

```typst
#set page(paper: "a5", numbering: "1")
#set heading(numbering: "1.")

= Lorem ipsum

#lorem(80)

== Mathematics

The sum of the first $n$ integers:

$ sum_(i=1)^n i = (n(n+1)) / 2 $

== A table

#table(
  columns: 3,
  [*Name*], [*Kind*], [*Size*],
  [alpha], [first], [1],
  [beta], [second], [22],
  [gamma], [third], [333],
)

== A figure

#figure(
  image("typst-figure.svg", width: 60%),
  caption: [An image read from a file next to the document.],
)

#include "typst-chapter.typ"

= Dolor sit amet

#lorem(300)
```

Run `cargo run -- --no-open examples/typst.typ`, open the printed URL and check that it renders without an error
banner. Kill the server afterwards.

- [ ] **Step 2: `Cargo.toml` metadata**

```toml
description = "Live-reloading browser preview for a single Markdown or Typst file, with mermaid diagrams and cursor sync for Helix."
keywords = ["markdown", "typst", "preview", "helix", "live-reload"]
```

(crates.io allows at most five keywords, so `typst` replaces `mermaid`.)

- [ ] **Step 3: README**

Make these edits in `README.md`:

1. The intro paragraph's first sentence becomes: "A small, self-contained CLI that serves a live-reloading browser
   preview of a Markdown file, including [mermaid](https://mermaid.js.org/) diagrams, or of a
   [Typst](https://typst.app/) document."
2. Under `## Usage`, the bullet "Every mode that takes a file ignores anything that is not `.md` or `.markdown`…"
   becomes "Every mode that takes a file ignores anything that is not `.md`, `.markdown` or `.typ`…" (rest unchanged).
3. In the Helix section, "only acts on `.md` and `.markdown` files; anywhere else it refuses with `not a Markdown file:
   <name>`" becomes "only acts on `.md`, `.markdown` and `.typ` files; anywhere else it refuses with `not a Markdown or
   Typst file: <name>`".
4. Add a `### Typst` subsection before `### Helix`:

```markdown
### Typst

A `.typ` file is compiled in-process, with the typst compiler built into the binary, and shown as its pages, the
way the PDF would look. Saving the document, or any file it reads (`#include`, `#import`, images, data), reloads the
preview. While a save does not compile, the last pages that did stay up under the error list.

The document's directory is the project root, as with `typst compile`. System fonts are used, with typst's own
fonts as a fallback. `@preview` packages come from the same cache as the typst CLI and are downloaded on first use.

`--line N` and `C-s` scroll to the text a line produced. A line that produces no text of its own (a `#set` rule, an
image, an equation) scrolls to the nearest text before it, and lines inside an `#include`d file do not scroll.
```

5. Under `## How it works`, after the `render.rs` bullet, add:

```markdown
- `typeset.rs` compiles Typst documents with the embedded compiler, renders each page as an SVG, and lays invisible
  `data-sourcepos` markers over each page so scroll sync works as it does for Markdown. `https.rs` downloads
  `@preview` packages over rustls.
```

Run `mise run lint` and let rumdl reflow anything it flags (`mise run fix`).

- [ ] **Step 4: CLAUDE.md**

1. Overview: "serves a live-reloading browser preview of one Markdown file, with mermaid diagrams." becomes "serves a
   live-reloading browser preview of one Markdown file (with mermaid diagrams) or Typst document."
2. Architecture intro: "Request/reload flow across the five modules:" becomes "Request/reload flow across the
   modules:" (a count adding a module would falsify).
3. In the `main.rs` item: "refuses non-Markdown files" becomes "refuses files that are neither Markdown nor Typst
   (`render::kind`)".
4. In the `server.rs` item, the `/content` bullet becomes: "`/content` re-reads and re-renders the current file on
   every request, with no caching, and names it in `X-Mdpreviewer-File` (percent-encoded). A Typst document renders
   through the `typeset::Session` held in `Current::typst`, and the files that compile read are watched
   (`watch_dependencies`), so editing an include sends `reload`."
5. Add an item after `render.rs`:

```markdown
6. **`typeset.rs`** embeds the typst compiler (typst-kit's `FileStore`, `FontStore`, `SystemPackages`). A `Session`
   keeps the `World` between renders; `render` resets the file store so a save is seen, compiles, and returns the
   fragment plus the project files it read. Each page is an SVG inside `div.typst-page`, followed by
   `div.typst-line` markers positioned in percent of the page height and tagged `data-sourcepos="L:1-L:1"` from the
   glyph spans of the main file, so `findBlock` needs no Typst-specific code. A failed compile returns an error
   banner above the last successful pages. Fonts are scanned once per process; tests use only the embedded fonts.
   `https.rs` is the package downloader: typst-kit's `Downloader` trait on ureq with rustls (no OpenSSL).
```

6. In the "To try a change manually" paragraph, add after the list of example files: "`typst.typ` (pages, math, a
   table, a figure from `typst-figure.svg`, and an `#include` of `typst-chapter.typ`)".
7. In the Commands section, the MSRV mention "(`rust-version`, 1.88)" becomes "(`rust-version`, 1.99, the latest stable; bump it together with the mise pin)".

- [ ] **Step 5: Final verification**

Run:

```bash
cargo test
cargo clippy --all-targets --quiet -- -D warnings
mise run lint
cargo tree -i openssl-sys; cargo tree -i native-tls
```

Expected: tests and lints pass; both `cargo tree` commands report no match.

- [ ] **Step 6: Commit**

```bash
jj commit -m "docs: document Typst previews and add a Typst example"
```
