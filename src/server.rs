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
//!   GET /assets/app.css         page styling
//!   GET /assets/app.js          client (mermaid, live reload, scroll sync)
//!   GET /assets/github-markdown.css   vendored GitHub markdown theme
//!   GET /assets/mermaid.min.js        vendored mermaid.js

use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use notify::RecommendedWatcher;
use tiny_http::{Header, Request, Response, Server};

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

/// Something every open preview tab should hear about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The current document changed on disk, or another document became current.
    Reload,
    /// Scroll to the block containing this 1-based source line.
    Scroll(u32),
    /// A heartbeat comment, sent now to find out which tabs have closed.
    Ping,
}

/// Encode an event as a named Server-Sent Event.
fn sse_frame(event: Event) -> String {
    match event {
        Event::Reload => "event: reload\ndata:\n\n".to_owned(),
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
    /// Whether a control `quit` ends the process. Always true in production;
    /// the in-process integration tests set it false, for the same reason
    /// they disable `idle_grace`.
    #[cfg(unix)]
    pub exit_on_quit: bool,
}

/// Registered senders, one per connected SSE client. The dispatcher fans
/// events out to all of them.
type Clients = Arc<Mutex<Vec<Sender<Event>>>>;

/// The document being previewed, and the watch that reloads it.
struct Current {
    path: PathBuf,
    /// Held only to keep the watch alive; replacing it stops the old watch.
    _watcher: Option<RecommendedWatcher>,
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
    let watcher = start_watch(&file, &events_tx);
    let state = State {
        current: Arc::new(Mutex::new(Current {
            path: file,
            _watcher: watcher,
        })),
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
                    if exit_on_quit {
                        let (path, inode) = &quit_socket;
                        control::remove_socket_if_ours(path, *inode);
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
        {
            let mut current = self.current.lock().unwrap();
            if current.path != path {
                let watcher = start_watch(&path, &self.events_tx);
                // Replacing `Current` drops, and so stops, the previous watch.
                *current = Current {
                    path,
                    _watcher: watcher,
                };
                let _ = self.events_tx.send(Event::Reload);
            }
        }
        if let Some(line) = line {
            let _ = self.events_tx.send(Event::Scroll(line));
        }
        control::Reply::Ok {
            url: url.to_owned(),
            clients: self.live_clients(),
        }
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
    let path = request.url().split('?').next().unwrap_or("/");
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
        _ => {
            let response = Response::from_data(&b"not found"[..]).with_status_code(404);
            let _ = request.respond(response);
        }
    }
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
    use std::time::Duration;

    use super::{Event, IdleClock, percent_encode, sse_frame};

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
    }

    #[test]
    fn file_names_are_percent_encoded_for_the_header() {
        assert_eq!(percent_encode("notes-2026_v1.md"), "notes-2026_v1.md");
        assert_eq!(percent_encode("café notes.md"), "caf%C3%A9%20notes.md");
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
}
