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
