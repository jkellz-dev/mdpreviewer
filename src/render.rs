//! Markdown to HTML rendering.
//!
//! Fenced ```mermaid blocks are left as ordinary code blocks
//! (`<pre><code class="language-mermaid">`) and turned into diagrams by the
//! browser client, so no raw-HTML injection is needed here.

use comrak::html::escape;
use comrak::nodes::NodeValue;
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

    // Recognise a leading `---` YAML block so it is not parsed as a rule plus a
    // setext heading. comrak's HTML output drops it; we render it ourselves.
    options.extension.front_matter_delimiter = Some(FRONT_MATTER_DELIMITER.to_owned());

    let root = parse_document(&arena, markdown, &options);

    let mut html = String::new();
    if let Some(node) = root.first_child()
        && let NodeValue::FrontMatter(raw) = &node.data().value
    {
        render_front_matter(raw, &mut html);
    }
    format_html(root, &options, &mut html).expect("writing to a String cannot fail");
    html
}

const FRONT_MATTER_DELIMITER: &str = "---";

/// Render raw front matter (delimiter lines included, as comrak stores it) as a
/// collapsed YAML code block.
fn render_front_matter(raw: &str, html: &mut String) {
    let yaml = raw
        .trim()
        .strip_prefix(FRONT_MATTER_DELIMITER)
        .and_then(|rest| rest.strip_suffix(FRONT_MATTER_DELIMITER))
        .unwrap_or(raw)
        .trim_matches('\n');

    html.push_str("<details class=\"frontmatter\"><summary>Front matter</summary>\n");
    html.push_str("<pre><code class=\"language-yaml\">");
    escape(html, yaml).expect("writing to a String cannot fail");
    html.push_str("</code></pre></details>\n");
}

#[cfg(test)]
mod tests {
    use super::render_markdown;

    #[test]
    fn front_matter_renders_as_collapsed_yaml_block() {
        let html = render_markdown("---\ntitle: Hello\ntags: [a, b]\n---\n# Body\n");
        assert!(html.starts_with("<details class=\"frontmatter\">"), "{html}");
        assert!(html.contains("<code class=\"language-yaml\">title: Hello\ntags: [a, b]</code>"), "{html}");
        assert!(html.contains("<h1>Body</h1>"), "{html}");
        // The closing fence must not turn the YAML into a setext heading.
        assert!(!html.contains("<h2>"), "{html}");
        assert!(!html.contains("---"), "{html}");
    }

    #[test]
    fn front_matter_is_escaped() {
        let html = render_markdown("---\ndesc: \"<b>&</b>\"\n---\n");
        assert!(html.contains("desc: &quot;&lt;b&gt;&amp;&lt;/b&gt;&quot;"), "{html}");
    }

    #[test]
    fn documents_without_front_matter_are_unchanged() {
        let html = render_markdown("# Title\n\ntext\n");
        assert_eq!(html, "<h1>Title</h1>\n<p>text</p>\n");
    }

    #[test]
    fn mid_document_rule_is_not_front_matter() {
        let html = render_markdown("intro\n\n---\n\nmore\n");
        assert!(!html.contains("frontmatter"), "{html}");
        assert!(html.contains("<hr />"), "{html}");
    }
}
