//! mdpreview: a live-reloading browser preview for Markdown, with mermaid.
//!
//! Open mode, `mdpreview [--line N] [--no-open] <file>`: if a preview server is
//! already running for this user, tell it (over the control socket, see
//! `control.rs`) to show the file and scroll to the line; otherwise start one.
//! A new server binds a local port, the browser opens at that URL, and the
//! page reloads on save. On Unix the server detaches from the launching
//! process (for example Helix's `:sh`) so the caller returns immediately.
//!
//! Sync mode, `mdpreview --sync [--line N] <file>`: only tell a running server
//! to show the file and scroll. It never starts a server, opens a tab, or
//! prints anything, so it is cheap enough to run on every save.
//!
//! Quit mode, `mdpreview --quit`: ask a running server to exit. Restart mode,
//! `mdpreview --restart [--line N] <file>`: quit, wait for the socket, then
//! open. Browser assets are compiled into the binary, so a restart is what
//! makes a new build take effect.

#[cfg(unix)]
mod control;
mod render;
mod server;
#[cfg(all(test, unix))]
mod testutil;
mod watch;

use std::io::IsTerminal;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

const USAGE: &str = "usage: mdpreview [--line N] [--no-open] <file.md>\n       \
                     mdpreview --sync [--line N] <file.md>\n       \
                     mdpreview --restart [--line N] [--no-open] <file.md>\n       \
                     mdpreview --quit";

/// Exit the server this long after the last browser tab closes.
const IDLE_GRACE: Duration = Duration::from_secs(15);

/// How long open mode waits for a running server before giving up.
#[cfg(unix)]
const OPEN_TIMEOUT: Duration = Duration::from_secs(1);

/// How long sync mode waits: it runs on every save, so it must stay snappy.
#[cfg(unix)]
const SYNC_TIMEOUT: Duration = Duration::from_millis(500);

/// How long `--restart` waits for the old server to release the socket, and
/// how often it looks.
#[cfg(unix)]
const RESTART_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(unix)]
const RESTART_STEP: Duration = Duration::from_millis(20);

/// What this invocation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Show a file, starting a server if none is running.
    Open,
    /// Only update a running server.
    Sync,
    /// Stop a running server.
    Quit,
    /// Stop a running server, then open.
    Restart,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    /// The file to show. `None` only in [`Mode::Quit`], which takes no file.
    file: Option<String>,
    line: Option<u32>,
    mode: Mode,
    no_open: bool,
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(raw.iter().cloned()) {
        Ok(args) => args,
        // Sync mode runs on every save; it never complains.
        Err(_) if raw.iter().any(|arg| arg == "--sync") => process::exit(0),
        Err(msg) => {
            eprintln!("mdpreview: {msg}\n{USAGE}");
            process::exit(2);
        }
    };
    match args.mode {
        Mode::Sync => {
            run_sync(&args);
            process::exit(0);
        }
        Mode::Quit => {
            run_quit();
            process::exit(0);
        }
        // Refuse before restarting, so a stray keypress in a source file
        // cannot take the preview server down with it.
        Mode::Restart | Mode::Open if !is_markdown(args.file.as_deref().unwrap_or_default()) => {
            fail(&format!(
                "not a Markdown file: {}",
                args.file.as_deref().unwrap_or_default()
            ));
        }
        Mode::Restart => run_restart(&args),
        Mode::Open => run_open(&args),
    }
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut file = None;
    let mut line = None;
    let mut mode = Mode::Open;
    let mut no_open = false;
    while let Some(arg) = args.next() {
        let next_mode = match arg.as_str() {
            "--sync" => Some(Mode::Sync),
            "--quit" => Some(Mode::Quit),
            "--restart" => Some(Mode::Restart),
            _ => None,
        };
        if let Some(next_mode) = next_mode {
            if mode != Mode::Open {
                return Err("--sync, --quit and --restart are mutually exclusive".into());
            }
            mode = next_mode;
        } else if arg == "--no-open" {
            no_open = true;
        } else if arg == "--line" {
            let value = args.next().ok_or("--line needs a value")?;
            line = Some(
                value
                    .parse()
                    .map_err(|_| format!("invalid line number: {value}"))?,
            );
        } else if arg.starts_with("--") {
            return Err(format!("unknown option: {arg}"));
        } else if file.is_some() {
            return Err(format!("unexpected argument: {arg}"));
        } else {
            file = Some(arg);
        }
    }
    match mode {
        Mode::Quit if file.is_some() => return Err("--quit takes no file".into()),
        Mode::Quit => {}
        _ if file.is_none() => return Err("missing file".into()),
        _ => {}
    }
    Ok(Args {
        file,
        line,
        mode,
        no_open,
    })
}

/// Expand a leading `~`. Helix's `%{buffer_name}` is home-relative for files
/// outside its working directory, and nothing expands that on the way here:
/// the binding quotes it, so the shell leaves it alone, and `~` is an ordinary
/// directory name to the OS.
fn expand_tilde(file: &str) -> PathBuf {
    let Some(rest) = file.strip_prefix('~') else {
        return PathBuf::from(file);
    };
    // `~user` names someone else's home, which only the shell can resolve.
    if !(rest.is_empty() || rest.starts_with('/')) {
        return PathBuf::from(file);
    }
    match std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        Some(home) => {
            let mut path = PathBuf::from(home);
            // `rest` starts with `/`, and pushing that would discard the home.
            path.push(rest.trim_start_matches('/'));
            path
        }
        None => PathBuf::from(file),
    }
}

/// Every mode that takes a file only acts on Markdown: `C-s` and the preview
/// bindings run for whatever buffer is open, including source files.
fn is_markdown(file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

/// The URL to open in a browser. `#line=N` tells a freshly opened tab where to
/// scroll, since it was not connected yet when the editor asked.
fn page_url(url: &str, line: Option<u32>) -> String {
    match line {
        Some(line) => format!("{url}#line={line}"),
        None => url.to_owned(),
    }
}

#[cfg(unix)]
fn run_sync(args: &Args) {
    let Some(file) = &args.file else {
        return;
    };
    if !is_markdown(file) {
        return;
    }
    let Ok(path) = expand_tilde(file).canonicalize() else {
        return;
    };
    let socket = control::default_socket_path();
    // Never talk to a socket in a directory another user could control.
    if control::ensure_socket_dir(&socket, control::current_uid()).is_err() {
        return;
    }
    let request = control::Request::Open(control::Open {
        path,
        line: args.line,
    });
    let _ = control::send(&socket, &request, SYNC_TIMEOUT);
}

#[cfg(not(unix))]
fn run_sync(_args: &Args) {}

/// Ask a running server to exit. Returns whether one acknowledged.
#[cfg(unix)]
/// What asking the running server to quit did.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum Stop {
    /// It acknowledged and is going away.
    Stopped,
    /// Nothing was listening.
    NoServer,
    /// Something answered but would not quit. In practice that is a server
    /// from a build that predates `--quit`; it has to be killed by PID.
    Refused,
}

#[cfg(unix)]
fn stop_server() -> Stop {
    let socket = control::default_socket_path();
    if control::ensure_socket_dir(&socket, control::current_uid()).is_err() {
        return Stop::NoServer;
    }
    classify(control::send(
        &socket,
        &control::Request::Quit,
        OPEN_TIMEOUT,
    ))
}

/// How a reply to `quit` maps onto [`Stop`].
#[cfg(unix)]
fn classify(result: Result<control::Reply, control::SendError>) -> Stop {
    match result {
        Ok(control::Reply::Bye) => Stop::Stopped,
        Err(control::SendError::NoServer) => Stop::NoServer,
        // An `err` reply, an `ok` reply (the verb was misread) or a timeout all
        // mean something is there that did not go away.
        _ => Stop::Refused,
    }
}

/// `--quit`. Silent unless stdout is a terminal, so it is safe on an editor
/// keybinding, and finding no server is not an error.
#[cfg(unix)]
fn run_quit() {
    report(match stop_server() {
        Stop::Stopped => "stopped the preview server",
        Stop::NoServer => "no preview server running",
        Stop::Refused => fail(REFUSED),
    });
}

#[cfg(not(unix))]
fn run_quit() {}

/// `--restart`: stop the running server, wait for it to let go of the control
/// socket so the new one can bind it, then open as usual.
#[cfg(unix)]
fn run_restart(args: &Args) {
    match stop_server() {
        Stop::Stopped => {
            let socket = control::default_socket_path();
            let deadline = std::time::Instant::now() + RESTART_TIMEOUT;
            while control::is_listening(&socket) && std::time::Instant::now() < deadline {
                std::thread::sleep(RESTART_STEP);
            }
        }
        // Opening would just hand the file to the server that refused, which
        // looks like the restart did nothing. Say so instead.
        Stop::Refused => fail(REFUSED),
        Stop::NoServer => {}
    }
    run_open(args);
}

#[cfg(not(unix))]
fn run_restart(args: &Args) {
    run_open(args);
}

fn run_open(args: &Args) {
    let file = args.file.as_deref().unwrap_or_default();
    let file = expand_tilde(file).canonicalize().unwrap_or_else(|err| {
        eprintln!("mdpreview: cannot open {file}: {err}");
        process::exit(1);
    });

    // Hand the file to a running server if there is one (exits), otherwise
    // claim the control socket for the server we are about to start.
    #[cfg(unix)]
    let control = reuse_or_bind(&file, args);

    // Bind before any fork so the browser can connect via the kernel's accept
    // backlog even before the server thread starts accepting.
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("mdpreview: cannot bind local port: {err}");
            process::exit(1);
        }
    };
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    let url = format!("http://127.0.0.1:{port}/");

    let config = server::Config {
        url: url.clone(),
        idle_grace: Some(IDLE_GRACE),
        #[cfg(unix)]
        control,
        #[cfg(unix)]
        exit_on_quit: true,
    };

    if detach(&url, args) {
        // Only the process that should serve reaches here.
        run_server(listener, file, config);
    }
}

/// Ask a running server to show `file`. If it does, point the browser at it
/// when no tab is listening (or print the URL with `--no-open`) and exit.
/// If no server is running, bind the control socket for a new one; if the
/// socket cannot be used, warn and return `None` for a standalone server.
#[cfg(unix)]
fn reuse_or_bind(file: &Path, args: &Args) -> Option<control::ControlSocket> {
    let socket = control::default_socket_path();
    // Check the directory before connecting: a socket planted by another user
    // could learn our paths and hand back a URL for us to open.
    if let Err(err) = control::ensure_socket_dir(&socket, control::current_uid()) {
        eprintln!(
            "mdpreview: cannot use {} ({err}); running a standalone preview",
            socket.display()
        );
        return None;
    }
    let request = control::Request::Open(control::Open {
        path: file.to_owned(),
        line: args.line,
    });
    match control::send(&socket, &request, OPEN_TIMEOUT) {
        Ok(control::Reply::Ok { url, clients }) => {
            if args.no_open {
                println!("{url}");
            } else if clients == 0 {
                let _ = open::that(page_url(&url, args.line));
            }
            process::exit(0);
        }
        Ok(control::Reply::Bye) => {
            // Only a `quit` is answered with `bye`; a server that sends one
            // here is not speaking this protocol.
            eprintln!("mdpreview: unexpected reply from the control socket");
            process::exit(1);
        }
        Ok(control::Reply::Err(reason)) => {
            eprintln!("mdpreview: {reason}");
            process::exit(1);
        }
        Err(control::SendError::NoServer) => {}
        Err(err @ control::SendError::Unusable(_)) => {
            eprintln!("mdpreview: {err}; running a standalone preview");
            return None;
        }
        Err(err) => {
            eprintln!("mdpreview: {err}");
            process::exit(1);
        }
    }
    match control::bind(&socket) {
        Ok(control) => Some(control),
        Err(err) => {
            eprintln!(
                "mdpreview: cannot use {} ({err}); running a standalone preview",
                socket.display()
            );
            None
        }
    }
}

/// Detach the server from the launching process.
///
/// On Unix: fork; the parent announces the URL (see [`announce`]) and exits,
/// while the child starts a new session and redirects its standard streams to
/// `/dev/null` (so the launcher's pipe sees EOF). Returns `true` in the process
/// that should run the server.
///
/// On other platforms: announce in-process and run in the foreground.
#[cfg(unix)]
fn detach(url: &str, args: &Args) -> bool {
    match unsafe { libc::fork() } {
        -1 => {
            eprintln!("mdpreview: fork failed");
            process::exit(1);
        }
        0 => {
            detach_child();
            true
        }
        _ => {
            announce(url, args);
            process::exit(0);
        }
    }
}

#[cfg(not(unix))]
fn detach(url: &str, args: &Args) -> bool {
    announce(url, args);
    true
}

/// Open the browser at the preview (scrolled to `--line`) and report the URL.
/// With `--no-open`, only print the URL, even when stdout is not a terminal.
fn announce(url: &str, args: &Args) {
    if args.no_open {
        println!("{url}");
        return;
    }
    let _ = open::that(page_url(url, args.line));
    report(url);
}

/// A server that answers but will not quit predates `--quit`, so there is no
/// way to ask it to go; it has to be killed by PID.
#[cfg(unix)]
const REFUSED: &str =
    "the running preview server is too old to quit; kill it by PID (ps | grep mdpreview)";

/// Report a failure and stop. Unlike [`report`] this is never suppressed: an
/// editor that runs `mdpreview` from a keybinding shows what a command wrote,
/// and a refusal with no explanation looks like a broken binding.
fn fail(message: &str) -> ! {
    eprintln!("mdpreview: {message}");
    process::exit(1);
}

/// Report success. Only when stdout is a real terminal: this is mostly the
/// URL, and an editor that runs `mdpreview` from a keybinding captures output
/// into a popup, which would then appear on every keypress. The work happens
/// either way. Failures go through [`fail`] and are never suppressed.
fn report(message: &str) {
    if std::io::stdout().is_terminal() {
        println!("{message}");
    }
}

/// Put the child in its own session and detach its standard streams.
#[cfg(unix)]
fn detach_child() {
    unsafe {
        libc::setsid();
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, libc::STDIN_FILENO);
            libc::dup2(devnull, libc::STDOUT_FILENO);
            libc::dup2(devnull, libc::STDERR_FILENO);
            if devnull > libc::STDERR_FILENO {
                libc::close(devnull);
            }
        }
    }
}

/// Start the HTTP server on `listener` and serve until the process exits.
fn run_server(listener: TcpListener, file: PathBuf, config: server::Config) {
    let server = match tiny_http::Server::from_listener(listener, None) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("mdpreview: cannot start server: {err}");
            process::exit(1);
        }
    };
    server::serve(server, file, config);
}

#[cfg(test)]
mod tests {
    use super::{Args, Mode, expand_tilde, is_markdown, page_url, parse_args};
    use std::path::PathBuf;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn open_mode_takes_a_file_and_optional_line() {
        assert_eq!(
            parse(&["notes.md"]),
            Ok(Args {
                file: Some("notes.md".into()),
                line: None,
                mode: Mode::Open,
                no_open: false
            })
        );
        assert_eq!(
            parse(&["--line", "42", "--no-open", "my notes.md"]),
            Ok(Args {
                file: Some("my notes.md".into()),
                line: Some(42),
                mode: Mode::Open,
                no_open: true
            })
        );
    }

    #[test]
    fn sync_mode_is_a_flag() {
        assert_eq!(
            parse(&["--sync", "--line", "7", "a.md"]),
            Ok(Args {
                file: Some("a.md".into()),
                line: Some(7),
                mode: Mode::Sync,
                no_open: false
            })
        );
    }

    #[test]
    fn quit_mode_takes_no_file() {
        assert_eq!(
            parse(&["--quit"]),
            Ok(Args {
                file: None,
                line: None,
                mode: Mode::Quit,
                no_open: false
            })
        );
        assert!(parse(&["--quit", "a.md"]).is_err());
    }

    #[test]
    fn restart_mode_still_needs_a_file() {
        assert_eq!(
            parse(&["--restart", "--line", "3", "a.md"]),
            Ok(Args {
                file: Some("a.md".into()),
                line: Some(3),
                mode: Mode::Restart,
                no_open: false
            })
        );
        assert!(parse(&["--restart"]).is_err());
    }

    #[test]
    fn bad_arguments_are_rejected() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["--line"]).is_err());
        assert!(parse(&["--line", "x", "a.md"]).is_err());
        assert!(parse(&["--line", "-1", "a.md"]).is_err());
        assert!(parse(&["--bogus", "a.md"]).is_err());
        assert!(parse(&["a.md", "b.md"]).is_err());
        // The modes are mutually exclusive.
        assert!(parse(&["--sync", "--restart", "a.md"]).is_err());
        assert!(parse(&["--sync", "--quit"]).is_err());
        assert!(parse(&["--restart", "--quit", "a.md"]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_server_that_will_not_quit_is_not_a_missing_server() {
        use super::{Stop, classify};
        use crate::control::{Reply, SendError};

        assert_eq!(classify(Ok(Reply::Bye)), Stop::Stopped);
        assert_eq!(classify(Err(SendError::NoServer)), Stop::NoServer);
        // A build without `--quit` answers `err`; older still, it may misread
        // the verb and answer `ok`. Neither stopped anything.
        assert_eq!(
            classify(Ok(Reply::Err("bad request".into()))),
            Stop::Refused
        );
        assert_eq!(
            classify(Ok(Reply::Ok {
                url: "u".into(),
                clients: 0
            })),
            Stop::Refused
        );
        assert_eq!(classify(Err(SendError::Timeout)), Stop::Refused);
    }

    #[test]
    fn only_markdown_files_are_synced() {
        for yes in ["a.md", "docs/README.MD", "notes.markdown", "/abs/x.Md"] {
            assert!(is_markdown(yes), "{yes}");
        }
        for no in ["main.rs", "[scratch]", "foo.md.bak", ".md", "Makefile", ""] {
            assert!(!is_markdown(no), "{no}");
        }
    }

    #[test]
    fn a_leading_tilde_becomes_the_home_directory() {
        // SAFETY: single-threaded test, and the value is restored below.
        let home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", "/home/me") };

        assert_eq!(
            expand_tilde("~/notes/a.md"),
            PathBuf::from("/home/me/notes/a.md")
        );
        assert_eq!(expand_tilde("~"), PathBuf::from("/home/me"));
        // Only a leading `~` path segment counts.
        assert_eq!(expand_tilde("~other/a.md"), PathBuf::from("~other/a.md"));
        assert_eq!(expand_tilde("notes/~/a.md"), PathBuf::from("notes/~/a.md"));
        assert_eq!(expand_tilde("/abs/a.md"), PathBuf::from("/abs/a.md"));
        assert_eq!(expand_tilde("a.md"), PathBuf::from("a.md"));

        unsafe {
            match home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn page_url_carries_the_line_as_a_fragment() {
        assert_eq!(
            page_url("http://127.0.0.1:1/", Some(12)),
            "http://127.0.0.1:1/#line=12"
        );
        assert_eq!(page_url("http://127.0.0.1:1/", None), "http://127.0.0.1:1/");
    }
}
