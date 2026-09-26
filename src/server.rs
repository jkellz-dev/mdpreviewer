//! HTTP server: static shell, rendered content, and a live-update event
//! stream. On Unix it also answers `open` requests on the control socket (see
//! `control.rs`), which switch the previewed document and scroll the page.
//!
//! Routes:
//!   GET /                       preview shell (HTML)
//!   GET /content                rendered markdown fragment; the
//!                               `X-Mdpreviewer-File` header carries the
//!                               percent-encoded file name
//!   GET /events                 Server-Sent Events: `reload` and `scroll`
//!   POST /open                  follow a relative Markdown link: the body is
//!                               the link as written, and the document it
//!                               names becomes the current one
//!   GET /file?path=<link>       an image the document refers to by a
//!                               relative path
//!   GET /assets/app.css         page styling
//!   GET /assets/app.js          client (mermaid, live reload, scroll sync)
//!   GET /assets/github-markdown.css   vendored GitHub markdown theme
//!   GET /assets/mermaid.min.js        vendored mermaid.js

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use notify::RecommendedWatcher;
use tiny_http::{Header, Method, Request, Response, Server};

#[cfg(unix)]
use crate::control;
use crate::{render, watch};

const SHELL_HTML: &str = include_str!("../assets/shell.html");
const APP_CSS: &str = include_str!("../assets/app.css");
const APP_JS: &str = include_str!("../assets/app.js");
const GITHUB_MARKDOWN_CSS: &str = include_str!("../assets/vendor/github-markdown.css");
const MERMAID_JS: &[u8] = include_bytes!("../assets/vendor/mermaid.min.js");

/// How long a `/events` read blocks before emitting a heartbeat. Heartbeats let
/// the server notice a disconnected client on the next write.
const SSE_HEARTBEAT: Duration = Duration::from_secs(10);

/// The pause after each probe ping in [`State::live_clients`]; plenty for a
/// loopback peer's reset to arrive.
const PROBE_GAP: Duration = Duration::from_millis(25);

/// The request header `POST /open` requires. A browser will not send a custom
/// header cross-origin without a CORS preflight, which this server never
/// grants, so another web page cannot switch the preview.
const LINK_HEADER: &str = "X-Mdpreviewer";

/// The longest link body `POST /open` reads.
const MAX_LINK_BYTES: u64 = 4096;

/// Something every open preview tab should hear about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The current document changed on disk, or another document became current.
    Reload,
    /// An image the current document shows changed on disk.
    Images,
    /// Scroll to the block containing this 1-based source line.
    Scroll(u32),
    /// A heartbeat comment, sent now to find out which tabs have closed.
    Ping,
}

/// Encode an event as a named Server-Sent Event.
fn sse_frame(event: Event) -> String {
    match event {
        Event::Reload => "event: reload\ndata:\n\n".to_owned(),
        Event::Images => "event: images\ndata:\n\n".to_owned(),
        Event::Scroll(line) => format!("event: scroll\ndata: {line}\n\n"),
        Event::Ping => ": ping\n\n".to_owned(),
    }
}

/// How [`serve`] runs.
pub struct Config {
    /// The preview's base URL, reported back to `open` requests.
    pub url: String,
    /// Exit this long after the last browser tab closes. `None` never exits,
    /// which tests rely on.
    pub idle_grace: Option<Duration>,
    /// Where to accept `open` requests. `None` runs a standalone server.
    #[cfg(unix)]
    pub control: Option<control::ControlSocket>,
    /// Whether a control `quit` ends the process once it has removed the
    /// socket. Always true in production; the in-process integration tests
    /// set it false, for the same reason they disable `idle_grace`.
    #[cfg(unix)]
    pub exit_on_quit: bool,
}

/// Registered senders, one per connected SSE client. The dispatcher fans
/// events out to all of them.
type Clients = Arc<Mutex<Vec<Sender<Event>>>>;

/// The document being previewed, the watch that reloads it, and the images
/// it has shown.
struct Current {
    path: PathBuf,
    /// Held only to keep the watch alive; replacing it stops the old watch.
    _watcher: Option<RecommendedWatcher>,
    /// Every image `/file` has served for this document, canonical. Replacing
    /// one sends [`Event::Images`], through `image_watcher`.
    images: BTreeSet<PathBuf>,
    image_watcher: Option<RecommendedWatcher>,
}

impl Current {
    fn new(path: PathBuf, events_tx: &Sender<Event>) -> Self {
        Current {
            _watcher: start_watch(&path, events_tx),
            path,
            images: BTreeSet::new(),
            image_watcher: None,
        }
    }
}

/// Shared server state passed to each request handler thread.
#[derive(Clone)]
struct State {
    current: Arc<Mutex<Current>>,
    /// Feeds the dispatcher; cloned into each new watcher.
    events_tx: Sender<Event>,
    clients: Clients,
    active: Arc<AtomicUsize>,
    ever_connected: Arc<AtomicBool>,
    /// Set by each `open` request, so the monitor restarts its countdown
    /// while a tab the CLI just opened is still connecting.
    opened: Arc<AtomicBool>,
}

/// Run the preview server until it shuts itself down (see [`spawn_monitor`]).
pub fn serve(server: Server, file: PathBuf, config: Config) {
    let (events_tx, events_rx) = channel::<Event>();
    let state = State {
        current: Arc::new(Mutex::new(Current::new(file, &events_tx))),
        events_tx,
        clients: Arc::new(Mutex::new(Vec::new())),
        active: Arc::new(AtomicUsize::new(0)),
        ever_connected: Arc::new(AtomicBool::new(false)),
        opened: Arc::new(AtomicBool::new(false)),
    };

    spawn_dispatcher(events_rx, Arc::clone(&state.clients));

    // Answer control requests, and remember the socket to remove it on exit.
    #[cfg(unix)]
    let socket = config.control.map(
        |control::ControlSocket {
             listener,
             path,
             inode,
         }| {
            let handler_state = state.clone();
            let url = config.url.clone();
            let quit_socket = (path.clone(), inode);
            let exit_on_quit = config.exit_on_quit;
            control::spawn_listener(
                listener,
                move |open| handler_state.open(open, &url),
                move || {
                    let (path, inode) = &quit_socket;
                    control::remove_socket_if_ours(path, *inode);
                    if exit_on_quit {
                        process::exit(0);
                    }
                },
            );
            (path, inode)
        },
    );

    if let Some(grace) = config.idle_grace {
        spawn_monitor(
            Arc::clone(&state.active),
            Arc::clone(&state.ever_connected),
            Arc::clone(&state.opened),
            grace,
            move || {
                #[cfg(unix)]
                if let Some((path, inode)) = &socket {
                    control::remove_socket_if_ours(path, *inode);
                }
            },
        );
    }

    for request in server.incoming_requests() {
        let state = state.clone();
        thread::spawn(move || handle(request, &state));
    }
}

impl State {
    /// Handle a control-socket `open`: make `path` the current document (if it
    /// is not already) and ask every tab to scroll to `line`.
    #[cfg(unix)]
    fn open(&self, open: control::Open, url: &str) -> control::Reply {
        let control::Open { path, line } = open;
        if !path.is_absolute() {
            return control::Reply::Err(format!("not an absolute path: {}", path.display()));
        }
        if !path.is_file() {
            return control::Reply::Err(format!("not a file: {}", path.display()));
        }
        self.opened.store(true, Ordering::Relaxed);
        self.switch_to(path);
        if let Some(line) = line {
            let _ = self.events_tx.send(Event::Scroll(line));
        }
        control::Reply::Ok {
            url: url.to_owned(),
            clients: self.live_clients(),
        }
    }

    /// Make `path` the current document, if it is not already, and tell every
    /// tab to reload.
    fn switch_to(&self, path: PathBuf) {
        let mut current = self.current.lock().unwrap();
        if current.path != path {
            // Replacing `Current` drops, and so stops, the previous watches.
            *current = Current::new(path, &self.events_tx);
            let _ = self.events_tx.send(Event::Reload);
        }
    }

    fn current_path(&self) -> PathBuf {
        self.current.lock().unwrap().path.clone()
    }

    /// Watch `image`, which `/file` just served for `doc`, unless it is
    /// already watched or `doc` stopped being current in the meantime. The
    /// watch is rebuilt over the whole set: a document shows few images, and
    /// one watch per directory is simpler than one per image.
    fn watch_image(&self, doc: &Path, image: PathBuf) {
        let mut current = self.current.lock().unwrap();
        if current.path != doc || !current.images.insert(image) {
            return;
        }
        let images: Vec<PathBuf> = current.images.iter().cloned().collect();
        current.image_watcher =
            match watch::watch_files(&images, Event::Images, self.events_tx.clone()) {
                Ok(watcher) => Some(watcher),
                Err(err) => {
                    eprintln!("mdpreviewer: image watch failed: {err}");
                    None
                }
            };
    }

    /// The number of connected tabs, after flushing out any that closed since
    /// the last heartbeat. A write to a closed peer succeeds once (the peer
    /// answers with a reset) and fails the next time, so two pings a moment
    /// apart make every closed tab drop out. Open mode relies on this count to
    /// decide whether to open a new tab.
    #[cfg(unix)]
    fn live_clients(&self) -> usize {
        for _ in 0..2 {
            if self.active.load(Ordering::Relaxed) == 0 {
                break;
            }
            let _ = self.events_tx.send(Event::Ping);
            thread::sleep(PROBE_GAP);
        }
        self.active.load(Ordering::Relaxed)
    }
}

/// Watch `path`, sending [`Event::Reload`] on changes. A failed watch only
/// disables live reload, so it is reported but not fatal.
fn start_watch(path: &Path, events_tx: &Sender<Event>) -> Option<RecommendedWatcher> {
    match watch::watch_file(path, events_tx.clone()) {
        Ok(watcher) => Some(watcher),
        Err(err) => {
            eprintln!("mdpreviewer: file watch failed, live reload disabled: {err}");
            None
        }
    }
}

/// Fan events from the watcher and control socket out to every connected SSE
/// client, pruning clients whose channel has closed.
fn spawn_dispatcher(events_rx: Receiver<Event>, clients: Clients) {
    thread::spawn(move || {
        while let Ok(event) = events_rx.recv() {
            let mut guard = clients.lock().unwrap();
            guard.retain(|tx| tx.send(event).is_ok());
        }
    });
}

/// Exit the process once every client has been gone for `grace`, so a closed
/// browser tab does not leave an orphaned server running forever. Only arms
/// after the first client has connected, so the browser has time to open.
/// An `open` request restarts the countdown. `on_exit` runs just before the
/// process exits.
fn spawn_monitor(
    active: Arc<AtomicUsize>,
    ever_connected: Arc<AtomicBool>,
    opened: Arc<AtomicBool>,
    grace: Duration,
    on_exit: impl Fn() + Send + 'static,
) {
    const STEP: Duration = Duration::from_secs(3);
    thread::spawn(move || {
        let mut clock = IdleClock::new(grace);
        loop {
            thread::sleep(STEP);
            let armed = ever_connected.load(Ordering::Relaxed);
            let active = active.load(Ordering::Relaxed);
            let opened = opened.swap(false, Ordering::Relaxed);
            if clock.tick(STEP, armed, active, opened) {
                on_exit();
                process::exit(0);
            }
        }
    });
}

/// The shutdown monitor's decision, kept apart from its thread and clock.
struct IdleClock {
    grace: Duration,
    idle: Duration,
}

impl IdleClock {
    fn new(grace: Duration) -> Self {
        IdleClock {
            grace,
            idle: Duration::ZERO,
        }
    }

    /// Account for `step` having passed, and return whether the server has now
    /// been idle for the grace period. `armed` is whether any client has ever
    /// connected, `active` how many are connected now, and `opened` whether an
    /// `open` request arrived during the step.
    fn tick(&mut self, step: Duration, armed: bool, active: usize, opened: bool) -> bool {
        if armed && active == 0 && !opened {
            self.idle += step;
            self.idle >= self.grace
        } else {
            self.idle = Duration::ZERO;
            false
        }
    }
}

fn handle(request: Request, state: &State) {
    let (path, query) = request.url().split_once('?').unwrap_or((request.url(), ""));
    let query = query.to_owned();
    match path {
        "/" => respond(request, SHELL_HTML.as_bytes(), "text/html; charset=utf-8"),
        "/content" => serve_content(request, state),
        "/events" => serve_events(request, state),
        "/assets/app.css" => respond(request, APP_CSS.as_bytes(), "text/css; charset=utf-8"),
        "/assets/app.js" => respond(request, APP_JS.as_bytes(), "text/javascript; charset=utf-8"),
        "/assets/github-markdown.css" => respond(
            request,
            GITHUB_MARKDOWN_CSS.as_bytes(),
            "text/css; charset=utf-8",
        ),
        "/assets/mermaid.min.js" => respond(request, MERMAID_JS, "text/javascript; charset=utf-8"),
        "/open" => follow_link(request, state),
        "/file" => serve_file(request, state, &query),
        _ => status(request, 404, "not found"),
    }
}

/// `POST /open`: follow a relative link to another Markdown document. The
/// page always lives at `/`, so the browser cannot resolve such a link
/// itself; the client sends it as written and it is resolved here, against
/// the current document's directory. The switch reaches every tab as a
/// `reload`.
fn follow_link(mut request: Request, state: &State) {
    if *request.method() != Method::Post {
        return status(request, 405, "use POST");
    }
    if !request.headers().iter().any(|h| h.field.equiv(LINK_HEADER)) {
        return status(request, 403, "missing X-Mdpreviewer header");
    }
    let mut link = String::new();
    let mut body = request.as_reader().take(MAX_LINK_BYTES);
    if body.read_to_string(&mut link).is_err() {
        return status(request, 400, "the link is not UTF-8");
    }
    let Some(path) = resolve_link(&state.current_path(), &link) else {
        return status(request, 404, "no such file");
    };
    // Checked on the resolved path, so a `.md` symlink to something else is
    // refused rather than rendered.
    if !render::is_markdown(&path) {
        return status(request, 400, "not a Markdown file");
    }
    state.switch_to(path);
    let _ = request.respond(Response::empty(204));
}

/// `GET /file?path=<link>`: an image the document refers to by a relative
/// path (the client rewrites each such `src` to this route). Only image types
/// are served, checked on the resolved path, so a link cannot reach anything
/// else the user can read. `sandbox` keeps an SVG opened on its own from
/// running script on this origin.
fn serve_file(request: Request, state: &State, query: &str) {
    let link = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("path="))
        .and_then(percent_decode);
    let doc = state.current_path();
    let served = link
        .and_then(|link| resolve_link(&doc, &link))
        .and_then(|path| Some((image_type(&path)?, File::open(&path).ok()?, path)));
    let Some((content_type, file, path)) = served else {
        return status(request, 404, "not found");
    };
    state.watch_image(&doc, path);
    let response = Response::from_file(file)
        .with_header(header("Content-Type", content_type))
        .with_header(header("Content-Security-Policy", "sandbox"))
        .with_header(header("X-Content-Type-Options", "nosniff"))
        // Revalidate. The client also changes the URL when an image is
        // replaced, since a page reuses an image it already has for a URL.
        .with_header(header("Cache-Control", "no-cache"));
    let _ = request.respond(response);
}

/// Resolve `link`, a relative reference as written in `doc` (so possibly
/// percent-encoded), against `doc`'s directory. `None` unless it names an
/// existing file. The result is canonical, so symlinks and `..` are resolved.
fn resolve_link(doc: &Path, link: &str) -> Option<PathBuf> {
    let link = percent_decode(link)?;
    let relative = Path::new(&link);
    if link.is_empty() || relative.has_root() {
        return None;
    }
    let path = doc.parent()?.join(relative).canonicalize().ok()?;
    path.is_file().then_some(path)
}

/// The content type to serve an image under, or `None` for anything that is
/// not an image.
fn image_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

/// Decode `%XY` escapes. A `%` that starts no valid escape is kept as written;
/// `None` if the result is not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            // Checked first: `from_str_radix` would also take a leading `+`.
            .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Respond with a status code and a short plain-text reason.
fn status(request: Request, code: u16, reason: &str) {
    let response = Response::from_string(reason).with_status_code(code);
    let _ = request.respond(response);
}

/// Render the current document, with its file name in `X-Mdpreviewer-File` for
/// the page title. Read errors are reported inline so the browser shows the
/// problem rather than a blank page.
fn serve_content(request: Request, state: &State) {
    // Clone the path so the lock is not held while reading and rendering.
    let path = state.current.lock().unwrap().path.clone();
    let body = match fs::read_to_string(&path) {
        Ok(markdown) => render::render_markdown(&markdown),
        Err(err) => format!(
            "<h1>mdpreviewer</h1><p>Could not read <code>{}</code>: {}</p>",
            path.display(),
            err
        ),
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

/// Percent-encode a file name for the ASCII-only `X-Mdpreviewer-File` header;
/// the client decodes it with `decodeURIComponent`.
fn percent_encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Respond with a fixed body and content type.
fn respond(request: Request, body: &[u8], content_type: &str) {
    let response =
        Response::from_data(body.to_vec()).with_header(header("Content-Type", content_type));
    let _ = request.respond(response);
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

/// Register a new SSE client and stream events to it until it disconnects.
///
/// tiny_http's chunked response writer buffers small writes, which would stall
/// an event stream, so we take the raw socket via `into_writer` and speak the
/// minimal SSE protocol ourselves, flushing after every event.
fn serve_events(request: Request, state: &State) {
    let (tx, rx) = channel::<Event>();
    state.clients.lock().unwrap().push(tx);
    state.ever_connected.store(true, Ordering::Relaxed);
    let _guard = ActiveGuard::new(Arc::clone(&state.active));

    let mut writer = request.into_writer();

    // A bodyless, unbounded response: no Content-Length and no chunked encoding,
    // so the browser reads events incrementally until the socket closes.
    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: text/event-stream\r\n\
                Cache-Control: no-cache\r\n\
                Connection: keep-alive\r\n\
                \r\n";
    if write_flush(&mut writer, head.as_bytes()).is_err() {
        return;
    }
    if write_flush(&mut writer, b": connected\n\n").is_err() {
        return;
    }

    loop {
        let frame = match rx.recv_timeout(SSE_HEARTBEAT) {
            Ok(event) => sse_frame(event),
            // Heartbeat comment; also how a dead socket is detected (write fails).
            Err(RecvTimeoutError::Timeout) => sse_frame(Event::Ping),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if write_flush(&mut writer, frame.as_bytes()).is_err() {
            break;
        }
    }
}

fn write_flush(writer: &mut dyn Write, bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(bytes)?;
    writer.flush()
}

/// Tracks the number of connected SSE clients, decrementing on drop so the
/// shutdown monitor can tell when the last client has gone.
struct ActiveGuard(Arc<AtomicUsize>);

impl ActiveGuard {
    fn new(active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::Relaxed);
        ActiveGuard(active)
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    use super::{
        Event, IdleClock, image_type, percent_decode, percent_encode, resolve_link, sse_frame,
    };
    #[cfg(unix)]
    use crate::testutil::TestDir;

    const STEP: Duration = Duration::from_secs(3);

    #[test]
    fn the_server_idles_out_after_the_grace_period() {
        let mut clock = IdleClock::new(Duration::from_secs(15));
        // Not armed until a first client connects.
        assert!(!clock.tick(STEP * 10, false, 0, false));
        for _ in 0..4 {
            assert!(!clock.tick(STEP, true, 0, false));
        }
        assert!(clock.tick(STEP, true, 0, false));
    }

    #[test]
    fn an_open_request_restarts_the_idle_countdown() {
        let mut clock = IdleClock::new(Duration::from_secs(15));
        for _ in 0..4 {
            assert!(!clock.tick(STEP, true, 0, false));
        }
        // The CLI was told there are no clients and is opening a tab: give it
        // the full grace period to connect.
        assert!(!clock.tick(STEP, true, 0, true));
        for _ in 0..4 {
            assert!(!clock.tick(STEP, true, 0, false));
        }
        assert!(clock.tick(STEP, true, 0, false));
    }

    #[test]
    fn reload_is_a_named_event_with_empty_data() {
        assert_eq!(sse_frame(Event::Reload), "event: reload\ndata:\n\n");
    }

    #[test]
    fn scroll_carries_the_line() {
        assert_eq!(sse_frame(Event::Scroll(42)), "event: scroll\ndata: 42\n\n");
        assert_eq!(sse_frame(Event::Images), "event: images\ndata:\n\n");
    }

    #[test]
    fn file_names_are_percent_encoded_for_the_header() {
        assert_eq!(percent_encode("notes-2026_v1.md"), "notes-2026_v1.md");
        assert_eq!(percent_encode("café notes.md"), "caf%C3%A9%20notes.md");
    }

    #[test]
    fn percent_decoding_undoes_the_encoding() {
        assert_eq!(
            percent_decode("caf%C3%A9%20notes.md").as_deref(),
            Some("café notes.md")
        );
        assert_eq!(
            percent_decode("..%2fimg%2Fa.png").as_deref(),
            Some("../img/a.png")
        );
        // A `%` that starts no escape is kept as written.
        assert_eq!(percent_decode("100%.md").as_deref(), Some("100%.md"));
        assert_eq!(percent_decode("%zz").as_deref(), Some("%zz"));
        assert_eq!(percent_decode("%+1").as_deref(), Some("%+1"));
        // Bytes that are not UTF-8 have no path to name.
        assert_eq!(percent_decode("%FF"), None);
    }

    #[cfg(unix)]
    #[test]
    fn links_resolve_against_the_document_directory() {
        let dir = TestDir::new("resolve");
        fs::create_dir_all(dir.join("docs")).unwrap();
        let doc = dir.join("docs/a.md");
        fs::write(&doc, "").unwrap();
        fs::write(dir.join("b.md"), "").unwrap();

        assert_eq!(resolve_link(&doc, "../b.md"), Some(dir.join("b.md")));
        assert_eq!(resolve_link(&doc, "a.md"), Some(doc.clone()));
        assert_eq!(resolve_link(&doc, "missing.md"), None);
        assert_eq!(resolve_link(&doc, ""), None);
        // A directory is not a document or an image.
        assert_eq!(resolve_link(&doc, ".."), None);
        // Only relative links; an absolute one is refused even if it exists.
        let absolute = dir.join("b.md").display().to_string();
        assert_eq!(resolve_link(&doc, &absolute), None);
    }

    #[test]
    fn only_images_have_a_served_type() {
        assert_eq!(image_type(Path::new("a/b.PNG")), Some("image/png"));
        assert_eq!(image_type(Path::new("b.jpeg")), Some("image/jpeg"));
        assert_eq!(image_type(Path::new("b.svg")), Some("image/svg+xml"));
        for no in ["b.md", "id_rsa", "b.txt", "b.png.bak"] {
            assert_eq!(image_type(Path::new(no)), None, "{no}");
        }
    }
}

/// End-to-end: a real server on a temp socket and a random port, driven the
/// way the CLI and the browser drive it. No fork, no browser, no idle exit.
#[cfg(all(test, unix))]
mod integration_tests {
    use std::fs;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::Duration;

    use super::{Config, serve};
    use crate::control::{self, Reply, Request};
    use crate::testutil::TestDir;

    const WAIT: Duration = Duration::from_secs(5);

    struct Preview {
        http: SocketAddr,
        socket: PathBuf,
    }

    fn start(dir: &TestDir, file: &Path) -> Preview {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let http = listener.local_addr().unwrap();
        let socket = dir.join("mdpreviewer.sock");
        let config = Config {
            url: format!("http://{http}/"),
            idle_grace: None,
            control: Some(control::bind(&socket).unwrap()),
            // A quit must not take the test runner with it.
            exit_on_quit: false,
        };
        let server = tiny_http::Server::from_listener(listener, None).unwrap();
        let file = file.to_owned();
        thread::spawn(move || serve(server, file, config));
        Preview { http, socket }
    }

    fn open(preview: &Preview, path: &Path, line: Option<u32>) -> Reply {
        let request = Request::Open(control::Open {
            path: path.to_owned(),
            line,
        });
        control::send(&preview.socket, &request, WAIT).unwrap()
    }

    fn get(preview: &Preview, path: &str) -> String {
        let mut stream = TcpStream::connect(preview.http).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    /// A raw `/events` connection, read frame by frame.
    struct Events {
        stream: TcpStream,
        buf: String,
    }

    impl Events {
        /// Connect and wait until the server has registered this client.
        fn connect(preview: &Preview) -> Self {
            let mut stream = TcpStream::connect(preview.http).unwrap();
            stream.set_read_timeout(Some(WAIT)).unwrap();
            write!(stream, "GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut events = Events {
                stream,
                buf: String::new(),
            };
            while !events.buf.contains(": connected\n\n") {
                events.fill();
            }
            let end = events.buf.find(": connected\n\n").unwrap() + ": connected\n\n".len();
            events.buf.drain(..end);
            events
        }

        /// Drain whatever is already queued, then wait for the stream to fall
        /// quiet. A watcher can emit a `reload` for the write that set the
        /// fixture up, because that write happens before the watch starts and
        /// macOS delivers such events to a stream created just afterwards.
        /// Both the initial watch and the one an `open` creates when it
        /// switches files are exposed to this. The reload is unrelated to what
        /// the open-request tests assert, so tests that expect a specific next
        /// frame settle the stream once the watch they care about is in place.
        fn settle(&mut self) {
            const QUIET: Duration = Duration::from_millis(250);
            self.stream.set_read_timeout(Some(QUIET)).unwrap();
            let mut chunk = [0u8; 1024];
            loop {
                match self.stream.read(&mut chunk) {
                    Ok(0) => panic!("event stream closed"),
                    Ok(_) => continue,
                    Err(err)
                        if matches!(
                            err.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(err) => panic!("event stream read failed: {err}"),
                }
            }
            self.buf.clear();
            self.stream.set_read_timeout(Some(WAIT)).unwrap();
        }

        fn fill(&mut self) {
            let mut chunk = [0u8; 1024];
            let n = self
                .stream
                .read(&mut chunk)
                .expect("timed out waiting for an event");
            assert!(n > 0, "event stream closed");
            self.buf.push_str(std::str::from_utf8(&chunk[..n]).unwrap());
        }

        /// The next event frame, skipping `: ping` heartbeats.
        fn next(&mut self) -> String {
            loop {
                if let Some(end) = self.buf.find("\n\n") {
                    let frame: String = self.buf.drain(..end + 2).collect();
                    if !frame.starts_with(':') {
                        return frame;
                    }
                } else {
                    self.fill();
                }
            }
        }
    }

    #[test]
    fn opening_the_current_file_scrolls_without_reloading() {
        let dir = TestDir::new("same-file");
        let a = dir.join("a.md");
        fs::write(&a, "# A\n").unwrap();
        let preview = start(&dir, &a);
        let mut events = Events::connect(&preview);
        events.settle();

        let reply = open(&preview, &a, Some(3));
        assert_eq!(
            reply,
            Reply::Ok {
                url: format!("http://{}/", preview.http),
                clients: 1
            }
        );
        assert_eq!(events.next(), "event: scroll\ndata: 3\n\n");

        // No line and no switch: nothing is sent, so the next frame is the
        // scroll from the request after it, not a reload.
        assert!(matches!(open(&preview, &a, None), Reply::Ok { .. }));
        assert!(matches!(open(&preview, &a, Some(5)), Reply::Ok { .. }));
        assert_eq!(events.next(), "event: scroll\ndata: 5\n\n");
    }

    #[test]
    fn opening_another_file_switches_the_document_and_the_watch() {
        let dir = TestDir::new("switch");
        let a = dir.join("a.md");
        let b = dir.join("b.md");
        fs::write(&a, "# A\n").unwrap();
        fs::write(&b, "# B\n").unwrap();
        let preview = start(&dir, &a);
        let mut events = Events::connect(&preview);

        assert!(matches!(open(&preview, &b, Some(1)), Reply::Ok { .. }));
        assert_eq!(events.next(), "event: reload\ndata:\n\n");
        assert_eq!(events.next(), "event: scroll\ndata: 1\n\n");

        let content = get(&preview, "/content");
        // Header name case is up to tiny_http; the value is what matters.
        assert!(
            content
                .to_ascii_lowercase()
                .contains("x-mdpreviewer-file: b.md"),
            "{content}"
        );
        assert!(
            content.contains("<h1 data-sourcepos=\"1:1-1:3\">B</h1>"),
            "{content}"
        );

        // Drop whatever macOS replayed into the watch this switch created, so
        // the reload below is provably the write's. Settling before the write
        // cannot swallow that reload, and a drain that took too much would
        // hang this assertion rather than let it pass quietly.
        events.settle();

        // The new file is watched. That the old one no longer is rests on the
        // watch ending with its watcher, which `watch::tests` checks directly;
        // asserting it here would mean waiting for a reload not to arrive,
        // which macOS event replay makes a coin flip rather than a test.
        fs::write(&b, "# B changed\n").unwrap();
        assert_eq!(events.next(), "event: reload\ndata:\n\n");
    }

    #[test]
    fn a_closed_tab_is_not_counted_as_a_client() {
        let dir = TestDir::new("closed-tab");
        let a = dir.join("a.md");
        fs::write(&a, "# A\n").unwrap();
        let preview = start(&dir, &a);
        let events = Events::connect(&preview);
        drop(events); // The tab closes; the heartbeat is still seconds away.

        // Open mode opens a new tab only for zero clients, so this must be
        // exact right away rather than after the next failed heartbeat.
        match open(&preview, &a, None) {
            Reply::Ok { clients, .. } => assert_eq!(clients, 0),
            reply => panic!("expected ok, got {reply:?}"),
        }
    }

    #[test]
    fn opening_a_missing_file_is_an_error() {
        let dir = TestDir::new("missing");
        let a = dir.join("a.md");
        fs::write(&a, "# A\n").unwrap();
        let preview = start(&dir, &a);

        match open(&preview, &dir.join("nope.md"), None) {
            Reply::Err(reason) => assert!(reason.starts_with("not a file"), "{reason}"),
            reply => panic!("expected an error, got {reply:?}"),
        }
    }

    #[test]
    fn malformed_control_requests_get_an_error_reply() {
        let dir = TestDir::new("malformed-server");
        let a = dir.join("a.md");
        fs::write(&a, "# A\n").unwrap();
        let preview = start(&dir, &a);

        let mut stream = UnixStream::connect(&preview.socket).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.write_all(b"open\trelative.md\n").unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        assert!(reply.starts_with("err\t"), "{reply:?}");
    }

    #[test]
    fn quit_removes_the_control_socket() {
        let dir = TestDir::new("quit-server");
        let a = dir.join("a.md");
        fs::write(&a, "# A\n").unwrap();
        let preview = start(&dir, &a);

        assert_eq!(
            control::send(&preview.socket, &Request::Quit, WAIT).unwrap(),
            Reply::Bye
        );
        // The socket goes after `bye` is written, so give the listener a moment.
        let deadline = std::time::Instant::now() + WAIT;
        while preview.socket.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!preview.socket.exists(), "quit left the socket behind");
        assert!(!control::is_listening(&preview.socket));
    }

    /// Send an HTTP request with extra header lines and a body, and return the
    /// raw response.
    fn request(preview: &Preview, method: &str, path: &str, headers: &str, body: &str) -> String {
        let mut stream = TcpStream::connect(preview.http).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             {headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        String::from_utf8_lossy(&response).into_owned()
    }

    /// Follow a link the way the client does.
    fn follow(preview: &Preview, link: &str) -> String {
        request(preview, "POST", "/open", "X-Mdpreviewer: 1\r\n", link)
    }

    fn status(response: &str) -> &str {
        response.split(' ').nth(1).unwrap_or_default()
    }

    #[test]
    fn a_relative_link_switches_the_document() {
        let dir = TestDir::new("link");
        fs::create_dir_all(dir.join("docs/sub")).unwrap();
        let a = dir.join("docs/a.md");
        fs::write(&a, "# A\n").unwrap();
        fs::write(dir.join("README.md"), "# Readme\n").unwrap();
        fs::write(dir.join("docs/sub/my notes.md"), "# Notes\n").unwrap();
        let preview = start(&dir, &a);
        let mut events = Events::connect(&preview);
        events.settle();

        // Resolved against the current document's directory, not the page's `/`.
        assert_eq!(status(&follow(&preview, "../README.md")), "204");
        assert_eq!(events.next(), "event: reload\ndata:\n\n");
        let content = get(&preview, "/content");
        assert!(
            content
                .to_ascii_lowercase()
                .contains("x-mdpreviewer-file: readme.md"),
            "{content}"
        );

        // Now relative to README.md's directory, and percent-encoded the way
        // comrak writes an `href`.
        assert_eq!(status(&follow(&preview, "docs/sub/my%20notes.md")), "204");
        let content = get(&preview, "/content");
        assert!(content.contains(">Notes</h1>"), "{content}");
    }

    #[test]
    fn a_link_that_cannot_be_followed_is_refused() {
        let dir = TestDir::new("bad-link");
        let a = dir.join("a.md");
        fs::write(&a, "# A\n").unwrap();
        fs::write(dir.join("b.md"), "# B\n").unwrap();
        fs::write(dir.join("notes.txt"), "text\n").unwrap();
        let preview = start(&dir, &a);

        assert_eq!(status(&follow(&preview, "gone.md")), "404");
        assert_eq!(status(&follow(&preview, "notes.txt")), "400");
        assert_eq!(
            status(&follow(&preview, &dir.join("b.md").display().to_string())),
            "404"
        );
        // Without the header, as a cross-site form post would arrive.
        assert_eq!(
            status(&request(&preview, "POST", "/open", "", "b.md")),
            "403"
        );
        assert_eq!(status(&request(&preview, "GET", "/open", "", "")), "405");
        // A CORS preflight gets no `Access-Control-Allow-*`, so it fails.
        let preflight = request(&preview, "OPTIONS", "/open", "", "");
        assert!(
            !preflight
                .to_ascii_lowercase()
                .contains("access-control-allow"),
            "{preflight}"
        );

        let content = get(&preview, "/content");
        assert!(content.contains(">A</h1>"), "{content}");
    }

    #[test]
    fn relative_images_are_served_from_the_document_directory() {
        let dir = TestDir::new("image");
        fs::create_dir_all(dir.join("docs")).unwrap();
        fs::create_dir_all(dir.join("img")).unwrap();
        let a = dir.join("docs/a.md");
        fs::write(&a, "# A\n").unwrap();
        fs::write(dir.join("img/dot.png"), b"not really a png").unwrap();
        fs::write(dir.join("docs/secret.txt"), "secret\n").unwrap();
        let preview = start(&dir, &a);

        // The client sends the `src` as written, URL-encoded as a query value.
        let image = get(&preview, "/file?path=..%2Fimg%2Fdot.png");
        assert_eq!(status(&image), "200", "{image}");
        let lower = image.to_ascii_lowercase();
        assert!(lower.contains("content-type: image/png"), "{image}");
        assert!(
            lower.contains("content-security-policy: sandbox"),
            "{image}"
        );
        assert!(image.ends_with("not really a png"), "{image}");

        // Only images, and only files that exist.
        assert_eq!(status(&get(&preview, "/file?path=secret.txt")), "404");
        assert_eq!(status(&get(&preview, "/file?path=gone.png")), "404");
        assert_eq!(status(&get(&preview, "/file")), "404");
    }

    #[test]
    fn replacing_a_served_image_sends_an_images_event() {
        let dir = TestDir::new("image-watch");
        fs::create_dir_all(dir.join("img")).unwrap();
        let a = dir.join("a.md");
        fs::write(&a, "![](img/dot.png)\n").unwrap();
        let png = dir.join("img/dot.png");
        fs::write(&png, b"one").unwrap();
        let preview = start(&dir, &a);
        let mut events = Events::connect(&preview);

        // Serving the image is what starts its watch.
        assert_eq!(status(&get(&preview, "/file?path=img%2Fdot.png")), "200");
        events.settle();

        fs::write(&png, b"two").unwrap();
        assert_eq!(events.next(), "event: images\ndata:\n\n");
    }

    #[test]
    fn switching_documents_drops_the_image_watch() {
        let dir = TestDir::new("image-switch");
        let a = dir.join("a.md");
        let b = dir.join("b.md");
        fs::write(&a, "![](dot.png)\n").unwrap();
        fs::write(&b, "# B\n").unwrap();
        let png = dir.join("dot.png");
        fs::write(&png, b"one").unwrap();
        let preview = start(&dir, &a);
        let mut events = Events::connect(&preview);
        assert_eq!(status(&get(&preview, "/file?path=dot.png")), "200");

        assert!(matches!(open(&preview, &b, None), Reply::Ok { .. }));
        events.settle();

        // b.md shows no image, so replacing a.md's is not news. A write to
        // b.md afterwards gives the stream a frame to show next; the pause
        // outlasts the debounce, so a stray `images` would come first.
        fs::write(&png, b"two").unwrap();
        thread::sleep(Duration::from_millis(300));
        fs::write(&b, "# B changed\n").unwrap();
        assert_eq!(events.next(), "event: reload\ndata:\n\n");
    }
}
