//! Markdown to HTML rendering.
//!
//! Fenced ```mermaid blocks are left as ordinary code blocks
//! (`<pre><code class="language-mermaid">`) and turned into diagrams by the
//! browser client, so no raw-HTML injection is needed here.

use comrak::{Arena, Options, format_html, parse_document};

/// Render a markdown document to an HTML fragment suitable for insertion into
/// the preview shell's `#content` element.
pub fn render_markdown(markdown: &str) -> String {
    let arena = Arena::new();
    let mut options = Options::default();

    // GitHub Flavored Markdown niceties.
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;

    // These files are the user's own, served on 127.0.0.1, so pass literal HTML
    // (for example <details>, <img>, <kbd>) through untouched for a faithful
    // preview instead of replacing it with "<!-- raw HTML omitted -->".
    options.render.r#unsafe = true;

    let root = parse_document(&arena, markdown, &options);

    let mut html = String::new();
    format_html(root, &options, &mut html).expect("writing to a String cannot fail");
    html
}
