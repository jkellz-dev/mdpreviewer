//! HTTP server: static shell, rendered content, and a live-reload event stream.
//!
//! Routes:
//!   GET /                       preview shell (HTML)
//!   GET /content                rendered markdown fragment
//!   GET /events                 Server-Sent Events reload stream
//!   GET /assets/app.css         page styling
//!   GET /assets/app.js          client (mermaid init + SSE reload)
//!   GET /assets/github-markdown.css   vendored GitHub markdown theme
//!   GET /assets/mermaid.min.js        vendored mermaid.js

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tiny_http::{Header, Request, Response, Server};

use crate::render;

const SHELL_HTML: &str = include_str!("../assets/shell.html");
const APP_CSS: &str = include_str!("../assets/app.css");
const APP_JS: &str = include_str!("../assets/app.js");
const GITHUB_MARKDOWN_CSS: &str = include_str!("../assets/vendor/github-markdown.css");
const MERMAID_JS: &[u8] = include_bytes!("../assets/vendor/mermaid.min.js");

/// How long a `/events` read blocks before emitting a heartbeat. Heartbeats let
/// the server notice a disconnected client on the next write.
const SSE_HEARTBEAT: Duration = Duration::from_secs(10);

/// Something every open preview tab should hear about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The current document changed on disk, or another document became current.
    Reload,
    /// Scroll to the block containing this 1-based source line.
    Scroll(u32),
}

/// Encode an event as a named Server-Sent Event.
fn sse_frame(event: Event) -> String {
    match event {
        Event::Reload => "event: reload\ndata:\n\n".to_owned(),
        Event::Scroll(line) => format!("event: scroll\ndata: {line}\n\n"),
    }
}

/// Registered senders, one per connected SSE client. The dispatcher fans reload
/// signals out to all of them.
type Clients = Arc<Mutex<Vec<Sender<Event>>>>;

/// Shared server state passed to each request handler thread.
#[derive(Clone)]
struct State {
    file: PathBuf,
    clients: Clients,
    active: Arc<AtomicUsize>,
    ever_connected: Arc<AtomicBool>,
}

/// Run the preview server until it shuts itself down (see [`spawn_monitor`]).
pub fn serve(server: Server, file: PathBuf, events_rx: Receiver<Event>) {
    let state = State {
        file,
        clients: Arc::new(Mutex::new(Vec::new())),
        active: Arc::new(AtomicUsize::new(0)),
        ever_connected: Arc::new(AtomicBool::new(false)),
    };

    spawn_dispatcher(events_rx, Arc::clone(&state.clients));
    spawn_monitor(Arc::clone(&state.active), Arc::clone(&state.ever_connected));

    for request in server.incoming_requests() {
        let state = state.clone();
        thread::spawn(move || handle(request, &state));
    }
}

/// Fan events from the watcher (and control socket) out to every connected SSE
/// client, pruning clients whose channel has closed.
fn spawn_dispatcher(events_rx: Receiver<Event>, clients: Clients) {
    thread::spawn(move || {
        while let Ok(event) = events_rx.recv() {
            let mut guard = clients.lock().unwrap();
            guard.retain(|tx| tx.send(event).is_ok());
        }
    });
}

/// Exit the process once every client has disconnected, so a closed browser tab
/// does not leave an orphaned server running forever. Only arms after the first
/// client has connected, so the browser has time to open.
fn spawn_monitor(active: Arc<AtomicUsize>, ever_connected: Arc<AtomicBool>) {
    const STEP: Duration = Duration::from_secs(3);
    const GRACE: Duration = Duration::from_secs(15);
    thread::spawn(move || {
        let mut idle = Duration::ZERO;
        loop {
            thread::sleep(STEP);
            let armed = ever_connected.load(Ordering::Relaxed);
            if armed && active.load(Ordering::Relaxed) == 0 {
                idle += STEP;
                if idle >= GRACE {
                    process::exit(0);
                }
            } else {
                idle = Duration::ZERO;
            }
        }
    });
}

fn handle(request: Request, state: &State) {
    let path = request.url().split('?').next().unwrap_or("/");
    match path {
        "/" => respond(request, SHELL_HTML.as_bytes(), "text/html; charset=utf-8"),
        "/content" => respond(
            request,
            render_content(state).as_bytes(),
            "text/html; charset=utf-8",
        ),
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

/// Read and render the watched file, reporting read errors inline so the browser
/// shows the problem rather than a blank page.
fn render_content(state: &State) -> String {
    match fs::read_to_string(&state.file) {
        Ok(markdown) => render::render_markdown(&markdown),
        Err(err) => format!(
            "<h1>mdpreview</h1><p>Could not read <code>{}</code>: {}</p>",
            state.file.display(),
            err
        ),
    }
}

/// Respond with a fixed body and content type.
fn respond(request: Request, body: &[u8], content_type: &str) {
    let header = Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
        .expect("valid content-type header");
    let response = Response::from_data(body.to_vec()).with_header(header);
    let _ = request.respond(response);
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
            Err(RecvTimeoutError::Timeout) => ": ping\n\n".to_owned(),
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
    use super::{Event, sse_frame};

    #[test]
    fn reload_is_a_named_event_with_empty_data() {
        assert_eq!(sse_frame(Event::Reload), "event: reload\ndata:\n\n");
    }

    #[test]
    fn scroll_carries_the_line() {
        assert_eq!(sse_frame(Event::Scroll(42)), "event: scroll\ndata: 42\n\n");
    }
}
