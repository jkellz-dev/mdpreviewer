// mdpreview client: render the markdown fragment, turn fenced mermaid code
// blocks into diagrams, live-reload on Server-Sent Events, and scroll to the
// editor's cursor line when the server asks.

mermaid.initialize({
  startOnLoad: false,
  theme: "dark",
  securityLevel: "loose",
});

const content = document.getElementById("content");

// A scroll requested this soon before a content load started (or while it
// ran) is re-applied once the load has rendered, instead of restoring the old
// scroll position. A save sends `scroll` at once and `reload` ~80ms later when
// the watcher fires; a file switch needs its reload before the target exists;
// and a first render of many diagrams can take longer than this on its own,
// which is why the age is measured from when the load starts.
const PENDING_SCROLL_MS = 1500;

// Elements that make good scroll targets. comrak also puts data-sourcepos on
// inline elements (em, code, a, ...); those are ignored.
const BLOCK_SELECTOR = [
  "h1", "h2", "h3", "h4", "h5", "h6", "p", "li", "pre", "blockquote",
  "table", "tr", "hr", "details", "ul", "ol", "section",
].map((tag) => `${tag}[data-sourcepos]`).join(",");

// The latest scroll request from the editor: { line, at }.
let pendingScroll = null;
// Number of content loads in flight.
let loading = 0;

// Replace each `<pre><code class="language-mermaid">` with a `<pre class="mermaid">`
// element and return the new elements for mermaid to render.
function collectMermaidBlocks() {
  const codes = content.querySelectorAll("pre > code.language-mermaid");
  const blocks = [];
  codes.forEach((code) => {
    const pre = code.parentElement;
    const target = document.createElement("pre");
    target.className = "mermaid";
    // Keep the source position so the diagram stays a scroll target.
    if (pre.dataset.sourcepos) target.dataset.sourcepos = pre.dataset.sourcepos;
    target.textContent = code.textContent;
    pre.replaceWith(target);
    blocks.push(target);
  });
  return blocks;
}

// Parse comrak's `data-sourcepos="L:C-L:C"` into start and end lines.
function sourceLines(el) {
  const match = /^(\d+):\d+-(\d+):\d+$/.exec(el.dataset.sourcepos);
  return match ? { start: Number(match[1]), end: Number(match[2]) } : null;
}

// The block for a 1-based source line: the innermost block containing it,
// else the last block starting before it (blank lines, raw HTML), else null.
function findBlock(line) {
  let innermost = null;
  let innermostSpan = Infinity;
  let before = null;
  for (const el of content.querySelectorAll(BLOCK_SELECTOR)) {
    const lines = sourceLines(el);
    if (!lines) continue;
    if (lines.start <= line) before = el;
    const span = lines.end - lines.start;
    // `<=` so that on a tie the later, more deeply nested element wins.
    if (lines.start <= line && line <= lines.end && span <= innermostSpan) {
      innermost = el;
      innermostSpan = span;
    }
  }
  return innermost ?? before;
}

// Center the block for `line` and briefly outline it.
function scrollToLine(line) {
  const el = findBlock(line);
  if (!el) {
    window.scrollTo({ top: 0, behavior: "smooth" });
    return;
  }
  el.scrollIntoView({ block: "center", behavior: "smooth" });
  el.classList.remove("mdpreview-target");
  void el.offsetWidth; // Force a reflow so a repeated highlight restarts.
  el.classList.add("mdpreview-target");
  el.addEventListener(
    "animationend",
    () => el.classList.remove("mdpreview-target"),
    { once: true },
  );
}

function requestScroll(line) {
  pendingScroll = { line, at: Date.now() };
  // A load in progress applies it once rendered (see loadContent).
  if (loading === 0) scrollToLine(line);
}

async function loadContent() {
  const startedAt = Date.now();
  const scrollY = window.scrollY;
  loading += 1;
  try {
    let response;
    let html;
    try {
      response = await fetch("/content", { cache: "no-store" });
      html = await response.text();
    } catch (err) {
      console.error("mdpreview: failed to fetch content", err);
      return;
    }

    const name = response.headers.get("X-Mdpreview-File");
    if (name) document.title = `${decodeURIComponent(name)} — mdpreview`;

    content.innerHTML = html;

    const blocks = collectMermaidBlocks();
    if (blocks.length > 0) {
      try {
        await mermaid.run({ nodes: blocks });
      } catch (err) {
        console.error("mdpreview: mermaid render failed", err);
      }
    }

    // Diagrams have their final size now, so the target's position is right.
    if (pendingScroll && pendingScroll.at >= startedAt - PENDING_SCROLL_MS) {
      scrollToLine(pendingScroll.line);
    } else {
      window.scrollTo(0, scrollY);
    }
  } finally {
    loading -= 1;
  }
}

// A freshly opened tab gets its first target as `#line=N`, since it was not
// connected yet when the editor asked. Drop the hash afterwards so a manual
// page reload does not jump back.
function takeLineFromHash() {
  const match = /^#line=(\d+)$/.exec(location.hash);
  if (!match) return;
  pendingScroll = { line: Number(match[1]), at: Date.now() };
  history.replaceState(null, "", location.pathname + location.search);
}

takeLineFromHash();
document.addEventListener("DOMContentLoaded", loadContent);

// Live updates. EventSource reconnects automatically if the connection drops.
const events = new EventSource("/events");
events.addEventListener("reload", () => loadContent());
events.addEventListener("scroll", (event) => {
  const line = Number(event.data);
  if (Number.isInteger(line) && line > 0) requestScroll(line);
});
events.onerror = () => {
  // Transient during reconnect; nothing to do.
};
