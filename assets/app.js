// mdpreview client: render the markdown fragment, turn fenced mermaid code
// blocks into diagrams, and live-reload on Server-Sent Events.

mermaid.initialize({
  startOnLoad: false,
  theme: "dark",
  securityLevel: "loose",
});

const content = document.getElementById("content");

// Replace each `<pre><code class="language-mermaid">` with a `<pre class="mermaid">`
// element and return the new elements for mermaid to render.
function collectMermaidBlocks() {
  const codes = content.querySelectorAll("pre > code.language-mermaid");
  const blocks = [];
  codes.forEach((code) => {
    const pre = code.parentElement;
    const target = document.createElement("pre");
    target.className = "mermaid";
    target.textContent = code.textContent;
    pre.replaceWith(target);
    blocks.push(target);
  });
  return blocks;
}

async function loadContent() {
  const scrollY = window.scrollY;
  let html;
  try {
    const response = await fetch("/content", { cache: "no-store" });
    html = await response.text();
  } catch (err) {
    console.error("mdpreview: failed to fetch content", err);
    return;
  }

  content.innerHTML = html;

  const blocks = collectMermaidBlocks();
  if (blocks.length > 0) {
    try {
      await mermaid.run({ nodes: blocks });
    } catch (err) {
      console.error("mdpreview: mermaid render failed", err);
    }
  }

  window.scrollTo(0, scrollY);
}

document.addEventListener("DOMContentLoaded", loadContent);

// Live reload. EventSource reconnects automatically if the connection drops.
const events = new EventSource("/events");
events.onmessage = () => loadContent();
events.onerror = () => {
  // Transient during reconnect; nothing to do.
};
