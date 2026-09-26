// mdpreviewer client: render the markdown fragment, turn fenced mermaid code
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
  .join(",");

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
  el.classList.remove("mdpreviewer-target");
  void el.offsetWidth; // Force a reflow so a repeated highlight restarts.
  el.classList.add("mdpreviewer-target");
  el.addEventListener("animationend", () => el.classList.remove("mdpreviewer-target"), {
    once: true,
  });
}

function requestScroll(line) {
  pendingScroll = { line, at: Date.now() };
  // A load in progress applies it once rendered (see loadContent).
  if (loading === 0) scrollToLine(line);
}

// A link or image source written relative to the document: no scheme, and not
// starting with `/`, `#` or `?`. The page always lives at `/`, so the browser
// would resolve these against the wrong directory; the server resolves them
// against the document's own (see followLink and resolveImages).
function isRelative(ref) {
  return ref !== "" && !/^[a-z][a-z0-9+.-]*:/i.test(ref) && !/^[/#?]/.test(ref);
}

// Bumped on every `images` event and carried in each `/file` URL. A page
// reuses an image it already has for a URL without asking the server, so a
// replaced image needs a new URL to show. Between replacements the URL stays
// the same, so a reload of the text does not refetch every image.
let imageVersion = 0;

function imageUrl(src) {
  return `/file?path=${encodeURIComponent(src)}&v=${imageVersion}`;
}

// Point relative images at `/file`, which serves them from the document's
// directory. This runs on the parsed fragment before it is inserted, so the
// browser never requests the unresolved URL. The `src` as written is kept for
// refreshImages.
function resolveImages(fragment) {
  for (const img of fragment.querySelectorAll("img[src]")) {
    const src = img.getAttribute("src");
    if (!isRelative(src)) continue;
    img.dataset.mdpreviewerSrc = src;
    img.setAttribute("src", imageUrl(src));
  }
}

// An image the document shows was replaced on disk: fetch them all again
// under a new URL. Only the images change, so the text does not flicker.
async function refreshImages() {
  imageVersion += 1;
  const images = [...content.querySelectorAll("img[data-mdpreviewer-src]")];
  for (const img of images) img.setAttribute("src", imageUrl(img.dataset.mdpreviewerSrc));
  // An open zoom shows a clone, which is re-cloned once the new pixels are in
  // so it is sized from them.
  await Promise.allSettled(images.map((img) => img.decode()));
  refreshZoom();
}

// Send links to other sites to a new tab, so following one does not navigate
// the preview away from the document. Relative Markdown links are followed in
// place by followLink.
function retargetExternalLinks() {
  for (const link of content.querySelectorAll("a[href]")) {
    const url = new URL(link.href, location.href);
    const web = url.protocol === "http:" || url.protocol === "https:";
    if (web && url.origin !== location.origin) {
      link.target = "_blank";
      link.rel = "noopener noreferrer";
    }
  }
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
      console.error("mdpreviewer: failed to fetch content", err);
      return;
    }

    const name = response.headers.get("X-Mdpreviewer-File");
    if (name) document.title = `${decodeURIComponent(name)} — mdpreviewer`;

    const template = document.createElement("template");
    template.innerHTML = html;
    resolveImages(template.content);
    content.replaceChildren(template.content);
    retargetExternalLinks();

    const blocks = collectMermaidBlocks();
    if (blocks.length > 0) {
      try {
        await mermaid.run({ nodes: blocks });
      } catch (err) {
        console.error("mdpreviewer: mermaid render failed", err);
      }
    }

    // Diagrams have their final size now, so the target's position is right.
    if (pendingScroll && pendingScroll.at >= startedAt - PENDING_SCROLL_MS) {
      scrollToLine(pendingScroll.line);
    } else {
      window.scrollTo(0, scrollY);
    }

    // An open zoom overlay holds a clone of the old document; re-point it.
    refreshZoom();
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

// How long a toast stays up before it fades out on its own.
const TOAST_MS = 4000;

const toast = document.createElement("div");
toast.className = "mdpreviewer-toast";
toast.setAttribute("role", "status");
toast.hidden = true;
document.body.append(toast);
let toastTimer = null;

// Show `message` briefly at the top of the window. A newer message replaces
// an older one and restarts the timer; a click dismisses it early.
function showToast(message) {
  toast.textContent = message;
  toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    toast.hidden = true;
  }, TOAST_MS);
}

toast.addEventListener("click", () => {
  clearTimeout(toastTimer);
  toast.hidden = true;
});

// The link as the reader would name the file: percent escapes decoded, so
// `my%20notes.md` reads as `my notes.md`.
function linkLabel(path) {
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}

// What to tell the reader when `/open` refuses a link: the server answers 404
// for a file that is not there and 400 for one that is not Markdown.
function openFailure(status, path) {
  const name = linkLabel(path);
  if (status === 404) return `No such file: ${name}`;
  if (status === 400) return `Not a Markdown file: ${name}`;
  return `Could not open ${name}`;
}

// Follow a relative link to another Markdown file by asking the server to
// switch to it; the switch comes back to this tab as a `reload`.
//
// Modified and middle clicks are caught too. Left to the browser they would
// open the link, resolved against `/`, in a new tab as a 404, and the server
// shows one document to every tab, so a second tab could not show a different
// file anyway. They switch this tab instead.
async function followLink(event) {
  // A left click of any kind arrives as `click`, a middle click as `auxclick`.
  if (event.button > 1 || event.defaultPrevented) return;
  const link = event.target.closest?.("a[href]");
  if (!link) return;
  const href = link.getAttribute("href");
  if (!isRelative(href)) return;
  const path = href.split(/[?#]/)[0];
  if (!/\.(md|markdown)$/i.test(path)) return;
  event.preventDefault();

  // Start the new document at the top. Line 0 comes before every block, so
  // scrolling to it goes to the top, and the reload picks it up like any
  // recent scroll request.
  const previous = pendingScroll;
  pendingScroll = { line: 0, at: Date.now() };
  let response;
  try {
    response = await fetch("/open", {
      method: "POST",
      headers: { "X-Mdpreviewer": "1" },
      body: path,
    });
  } catch (err) {
    // The server is gone (quit, or idled out behind a sleeping laptop).
    pendingScroll = previous;
    console.error(`mdpreviewer: could not open ${path}`, err);
    showToast(openFailure(null, path));
    return;
  }
  if (!response.ok) {
    pendingScroll = previous;
    console.error(`mdpreviewer: could not open ${path}: ${response.status}`);
    showToast(openFailure(response.status, path));
  }
}

content.addEventListener("click", followLink);
content.addEventListener("auxclick", followLink);

// ---------------------------------------------------------------------------
// Zoom overlay
// ---------------------------------------------------------------------------

// What a click can blow up. A mermaid diagram is matched by its <svg> rather
// than the <pre> so the clone scales losslessly.
const ZOOM_SELECTOR = "pre.mermaid svg, img, table, pre:not(.mermaid)";

const MIN_ZOOM = 0.1;
const MAX_ZOOM = 30;
// Fit scales up as well as down, but blowing a 100px icon up to the full
// window is just a blur, so cap what fitting alone will do.
const MAX_FIT_ZOOM = 6;
// A pointer that moved this far is a pan, not a click.
const PAN_SLOP_PX = 4;

const overlay = document.createElement("div");
overlay.className = "mdpreviewer-zoom";
overlay.hidden = true;
overlay.innerHTML =
  '<div class="markdown-body mdpreviewer-zoom-figure"></div>' +
  '<button type="button" class="mdpreviewer-zoom-close" title="Close (esc)">\u00d7</button>' +
  '<div class="mdpreviewer-zoom-hint">scroll: zoom \u00b7 drag: pan \u00b7 0: fit \u00b7 f: fullscreen \u00b7 esc: close</div>';
const figure = overlay.querySelector(".mdpreviewer-zoom-figure");
document.body.append(overlay);

// The open zoom: { key, scale, fit, tx, ty }, or null when closed. `key` is
// the source position used to find the element again after a reload.
let zoom = null;
// The in-progress pan, or null.
let pan = null;

function clamp(value, min, max) {
  return Math.min(Math.max(value, min), max);
}

// The source position identifying `el` across reloads: its own, else that of
// the nearest block that has one.
function zoomKey(el) {
  const anchor = el.closest("[data-sourcepos]");
  return anchor ? anchor.dataset.sourcepos : null;
}

// The element's full extent, which is larger than its box for an image shrunk
// to the column width, or for a code block or table that scrolls sideways.
function contentSize(el) {
  const rect = el.getBoundingClientRect();
  if (el instanceof HTMLImageElement && el.naturalWidth > 0) {
    return { width: el.naturalWidth, height: el.naturalHeight };
  }
  // SVG elements have no scrollWidth/scrollHeight in some browsers.
  return {
    width: Math.max(rect.width, Number(el.scrollWidth) || 0),
    height: Math.max(rect.height, Number(el.scrollHeight) || 0),
  };
}

// The scale at which `width` x `height` fills the window, with a small margin.
function fitScale(width, height) {
  if (!(width > 0) || !(height > 0)) return 1;
  const scale = Math.min((window.innerWidth * 0.96) / width, (window.innerHeight * 0.96) / height);
  return clamp(scale, MIN_ZOOM, MAX_FIT_ZOOM);
}

// Copy `el` into the overlay at its full size and return that size.
function showClone(el) {
  const { width, height } = contentSize(el);
  const clone = el.cloneNode(true);
  // A native image drag would fire pointercancel and abort a pan.
  if (clone instanceof HTMLImageElement) clone.draggable = false;
  clone.querySelectorAll?.("img").forEach((img) => {
    img.draggable = false;
  });
  // The page styles shrink diagrams and images to the column; undo that.
  clone.style.maxWidth = "none";
  clone.style.maxHeight = "none";
  clone.style.margin = "0";
  clone.style.overflow = "visible";
  clone.style.width = `${width}px`;
  if (el instanceof HTMLImageElement || el.tagName.toLowerCase() === "svg") {
    clone.style.height = `${height}px`;
  }
  figure.replaceChildren(clone);
  figure.style.width = `${width}px`;
  figure.style.height = `${height}px`;
  return { width, height };
}

function applyTransform() {
  if (!zoom) return;
  figure.style.transform =
    `translate(-50%, -50%) translate(${zoom.tx}px, ${zoom.ty}px) ` + `scale(${zoom.scale})`;
}

function openZoom(el) {
  const { width, height } = showClone(el);
  const fit = fitScale(width, height);
  zoom = {
    key: zoomKey(el),
    width,
    height,
    scale: fit,
    fit,
    tx: 0,
    ty: 0,
    openedAt: Date.now(),
  };
  applyTransform();
  overlay.hidden = false;
  // Stop the page behind the overlay from scrolling under the wheel.
  document.body.style.overflow = "hidden";
}

function closeZoom() {
  if (!zoom) return;
  zoom = null;
  pan = null;
  overlay.hidden = true;
  overlay.classList.remove("is-panning");
  figure.replaceChildren();
  document.body.style.overflow = "";
  if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
}

// After a reload the overlay holds a clone of the previous document. Re-point
// it at the same block in the new one, keeping the current zoom and pan.
function refreshZoom() {
  if (!zoom) return;
  const el = zoom.key
    ? [...content.querySelectorAll(ZOOM_SELECTOR)].find(
        (candidate) => zoomKey(candidate) === zoom.key,
      )
    : null;
  if (!el) {
    closeZoom();
    return;
  }
  const { width, height } = showClone(el);
  zoom.width = width;
  zoom.height = height;
  zoom.fit = fitScale(width, height);
  applyTransform();
}

function fitZoom() {
  if (!zoom) return;
  zoom.fit = fitScale(zoom.width, zoom.height);
  zoom.scale = zoom.fit;
  zoom.tx = 0;
  zoom.ty = 0;
  applyTransform();
}

// Scale by `factor`, keeping the content under (x, y) in the same place.
function zoomBy(factor, x, y) {
  if (!zoom) return;
  const next = clamp(zoom.scale * factor, MIN_ZOOM, MAX_ZOOM);
  const centerX = window.innerWidth / 2 + zoom.tx;
  const centerY = window.innerHeight / 2 + zoom.ty;
  const ratio = next / zoom.scale;
  zoom.tx = x - (x - centerX) * ratio - window.innerWidth / 2;
  zoom.ty = y - (y - centerY) * ratio - window.innerHeight / 2;
  zoom.scale = next;
  applyTransform();
}

// The element a click should blow up, or null. A mermaid block is matched by
// its <pre> so that clicking the padding around the diagram works too, but it
// resolves to the <svg>, which is what ZOOM_SELECTOR names and what scales
// losslessly.
function resolveZoomTarget(node) {
  const mermaid = node.closest?.("pre.mermaid");
  if (mermaid) return mermaid.querySelector("svg");
  return node.closest?.(ZOOM_SELECTOR) ?? null;
}

// Code blocks and tables hold text, and a double-click there selects a word,
// but its first click would already have opened the zoom. So they open only
// once this long has passed without a second click.
const DOUBLE_CLICK_MS = 300;
let pendingZoom = null;

content.addEventListener("click", (event) => {
  // Any click cancels a zoom still waiting to see whether this is a
  // double-click, including the second click of one.
  clearTimeout(pendingZoom);
  pendingZoom = null;
  // Leave modified clicks, links and text selection alone.
  if (event.button !== 0 || event.defaultPrevented || event.detail > 1) return;
  if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
  const target = resolveZoomTarget(event.target);
  // Covers both a zoomable element inside a link and a link inside a
  // zoomable table or code block.
  if (!target || event.target.closest?.("a")) return;
  if (!(window.getSelection()?.isCollapsed ?? true)) return;
  if (!target.matches("pre, table")) {
    openZoom(target);
    return;
  }
  pendingZoom = setTimeout(() => {
    pendingZoom = null;
    // A reload may have replaced the block in the meantime.
    if (target.isConnected) openZoom(target);
  }, DOUBLE_CLICK_MS);
});

overlay.querySelector(".mdpreviewer-zoom-close").addEventListener("click", closeZoom);

overlay.addEventListener(
  "wheel",
  (event) => {
    if (!zoom) return;
    event.preventDefault();
    // deltaMode 1 is lines, 2 is pages; normalise both to roughly pixels.
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? 400 : 1;
    zoomBy(Math.exp(-event.deltaY * unit * 0.0015), event.clientX, event.clientY);
  },
  { passive: false },
);

overlay.addEventListener("pointerdown", (event) => {
  if (!zoom || event.button !== 0) return;
  if (event.target.closest(".mdpreviewer-zoom-close")) return;
  pan = {
    id: event.pointerId,
    x: event.clientX,
    y: event.clientY,
    tx: zoom.tx,
    ty: zoom.ty,
    onFigure: figure.contains(event.target),
    moved: false,
  };
  overlay.setPointerCapture(event.pointerId);
});

overlay.addEventListener("pointermove", (event) => {
  if (!pan || event.pointerId !== pan.id) return;
  const dx = event.clientX - pan.x;
  const dy = event.clientY - pan.y;
  if (!pan.moved && Math.hypot(dx, dy) < PAN_SLOP_PX) return;
  if (!pan.moved) {
    // The pointer crossed the threshold, so this is a pan. The browser may
    // have selected a few characters on the way there; drop them.
    window.getSelection()?.removeAllRanges();
  }
  pan.moved = true;
  overlay.classList.add("is-panning");
  zoom.tx = pan.tx + dx;
  zoom.ty = pan.ty + dy;
  applyTransform();
});

overlay.addEventListener("pointerup", (event) => {
  if (!pan || event.pointerId !== pan.id) return;
  const { moved, onFigure } = pan;
  pan = null;
  overlay.classList.remove("is-panning");
  if (overlay.hasPointerCapture(event.pointerId)) {
    overlay.releasePointerCapture(event.pointerId);
  }
  // A click on the backdrop closes; one on the content is left alone so it
  // can be selected and copied. The grace period is so the second click of a
  // double-click in the document does not close what the first click opened.
  const settled = Date.now() - zoom.openedAt > 300;
  if (!moved && !onFigure && settled) closeZoom();
});

overlay.addEventListener("pointercancel", () => {
  pan = null;
  overlay.classList.remove("is-panning");
});

// Belt and braces with the `draggable` flags above: any native drag inside
// the overlay would cancel the pointer mid-pan.
overlay.addEventListener("dragstart", (event) => event.preventDefault());

overlay.addEventListener("dblclick", (event) => {
  if (!zoom) return;
  event.preventDefault();
  // Toggle between filling the window and the document's own scale.
  if (Math.abs(zoom.scale - zoom.fit) > 0.001) fitZoom();
  else zoomBy(1 / zoom.scale, event.clientX, event.clientY);
});

document.addEventListener("keydown", (event) => {
  if (!zoom || event.altKey || event.ctrlKey || event.metaKey) return;
  if (event.key === "Escape") {
    // In fullscreen the browser handles the first Escape itself.
    if (!document.fullscreenElement) closeZoom();
  } else if (event.key === "0") {
    fitZoom();
  } else if (event.key === "f") {
    if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
    else overlay.requestFullscreen?.().catch(() => {});
  } else if (event.key === "+" || event.key === "=") {
    zoomBy(1.2, window.innerWidth / 2, window.innerHeight / 2);
  } else if (event.key === "-") {
    zoomBy(1 / 1.2, window.innerWidth / 2, window.innerHeight / 2);
  } else {
    return;
  }
  event.preventDefault();
});

// Refit on resize (including entering or leaving fullscreen), but only while
// the view is untouched, so a deliberate zoom survives.
window.addEventListener("resize", () => {
  if (zoom && Math.abs(zoom.scale - zoom.fit) < 0.001) fitZoom();
});

// Live updates. EventSource reconnects automatically if the connection drops.
const events = new EventSource("/events");
events.addEventListener("reload", () => loadContent());
events.addEventListener("images", () => refreshImages());
events.addEventListener("scroll", (event) => {
  const line = Number(event.data);
  if (Number.isInteger(line) && line > 0) requestScroll(line);
});
events.onerror = () => {
  // Transient during reconnect; nothing to do.
};
