//! mdpreviewer: a live-reloading browser preview for Markdown, with mermaid.
//!
//! Open mode, `mdpreviewer [--line N] [--no-open] <file>`: if a preview server is
//! already running for this user, tell it (over the control socket, see
//! `control.rs`) to show the file and scroll to the line; otherwise start one.
//! A new server binds a local port, the browser opens at that URL, and the
//! page reloads on save. On Unix the server detaches from the launching
//! process (for example Helix's `:sh`) so the caller returns immediately.
//!
//! Sync mode, `mdpreviewer --sync [--line N] <file>`: only tell a running server
//! to show the file and scroll. It never starts a server, opens a tab, or
//! prints anything, so it is cheap enough to run on every save.
//!
//! Quit mode, `mdpreviewer --quit`: ask a running server to exit. Restart mode,
//! `mdpreviewer --restart [--line N] <file>`: quit, wait for the socket, then
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

const USAGE: &str = "usage: mdpreviewer [--line N] [--no-open] <file.md>\n       \
                     mdpreviewer --sync [--line N] <file.md>\n       \
                     mdpreviewer --restart [--line N] [--no-open] <file.md>\n       \
                     mdpreviewer --quit";

/// Printed under [`USAGE`] by `--help`.
const OPTIONS: &str = "options:
      --line N     scroll the preview to line N
      --no-open    start the server without opening a browser tab
      --sync       only update a running preview, and say nothing
      --restart    stop a running preview, then open a new one
      --quit       stop a running preview
  -h, --help       show this help
  -V, --version    show the version";

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

/// An option that answers and exits, without starting or contacting a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EarlyExit {
    Help,
    Version,
}

impl EarlyExit {
    /// What to print. Both go to stdout: they were asked for, unlike the URL,
    /// which stays quiet unless stdout is a terminal because an editor running
    /// this as a shell command would show it in a popup. No editor binding
    /// passes these.
    fn message(self) -> String {
        match self {
            EarlyExit::Help => format!("{USAGE}\n\n{OPTIONS}"),
            EarlyExit::Version => {
                format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
            }
        }
    }
}

/// Look for an option that should answer straight away.
///
/// These are found before the arguments are parsed, so `--help` explains
/// itself even when the rest of the command line is wrong, which is when it is
/// most worth having. Help beats version wherever the two appear.
fn early_exit(raw: &[String]) -> Option<EarlyExit> {
    let mut found = None;
    for arg in raw {
        match arg.as_str() {
            "-h" | "--help" => return Some(EarlyExit::Help),
            "-V" | "--version" => found = Some(EarlyExit::Version),
            _ => {}
        }
    }
    found
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if let Some(early) = early_exit(&raw) {
        println!("{}", early.message());
        process::exit(0);
    }
    let args = match parse_args(raw.iter().cloned()) {
        Ok(args) => args,
        // Sync mode runs on every save; it never complains.
        Err(_) if raw.iter().any(|arg| arg == "--sync") => process::exit(0),
        Err(msg) => {
            eprintln!("mdpreviewer: {msg}\n{USAGE}");
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
        Mode::Restart | Mode::Open
            if !render::is_markdown(Path::new(args.file.as_deref().unwrap_or_default())) =>
        {
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
    expand_home(file, std::env::var_os("HOME").as_deref())
}

/// [`expand_tilde`] against a given home directory, so tests need not touch
/// the process environment.
fn expand_home(file: &str, home: Option<&std::ffi::OsStr>) -> PathBuf {
    let Some(rest) = file.strip_prefix('~') else {
        return PathBuf::from(file);
    };
    // `~user` names someone else's home, which only the shell can resolve.
    if !(rest.is_empty() || rest.starts_with('/')) {
        return PathBuf::from(file);
    }
    match home.filter(|home| !home.is_empty()) {
        Some(home) => {
            let mut path = PathBuf::from(home);
            // `rest` starts with `/`, and pushing that would discard the home.
            path.push(rest.trim_start_matches('/'));
            path
        }
        None => PathBuf::from(file),
    }
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
    if !render::is_markdown(Path::new(file)) {
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
    /// The socket directory failed its safety check, so no server could be
    /// reached, or have bound there. Carries the reason.
    Unusable(String),
}

#[cfg(unix)]
fn stop_server() -> Stop {
    let socket = control::default_socket_path();
    if let Err(err) = control::ensure_socket_dir(&socket, control::current_uid()) {
        return Stop::Unusable(format!("control socket is unusable: {err}"));
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
        Stop::Unusable(reason) => fail(&reason),
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
            while control::is_listening(&socket) {
                if std::time::Instant::now() >= deadline {
                    // Opening now would hand the file to the old server.
                    fail(
                        "the running preview server did not exit; kill it by PID (ps | grep mdpreviewer)",
                    );
                }
                std::thread::sleep(RESTART_STEP);
            }
        }
        // Opening would just hand the file to the server that refused, which
        // looks like the restart did nothing. Say so instead.
        Stop::Refused => fail(REFUSED),
        // No server was reachable, so there is nothing to stop. Opening says
        // why the socket is unusable and runs a standalone preview.
        Stop::NoServer | Stop::Unusable(_) => {}
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
        eprintln!("mdpreviewer: cannot open {file}: {err}");
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
            eprintln!("mdpreviewer: cannot bind local port: {err}");
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
            "mdpreviewer: cannot use {} ({err}); running a standalone preview",
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
            eprintln!("mdpreviewer: unexpected reply from the control socket");
            process::exit(1);
        }
        Ok(control::Reply::Err(reason)) => {
            eprintln!("mdpreviewer: {reason}");
            process::exit(1);
        }
        Err(control::SendError::NoServer) => {}
        Err(err @ control::SendError::Unusable(_)) => {
            eprintln!("mdpreviewer: {err}; running a standalone preview");
            return None;
        }
        Err(err) => {
            eprintln!("mdpreviewer: {err}");
            process::exit(1);
        }
    }
    match control::bind(&socket) {
        Ok(control) => Some(control),
        Err(err) => {
            eprintln!(
                "mdpreviewer: cannot use {} ({err}); running a standalone preview",
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
            eprintln!("mdpreviewer: fork failed");
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
    "the running preview server is too old to quit; kill it by PID (ps | grep mdpreviewer)";

/// Report a failure and stop. Unlike [`report`] this is never suppressed: an
/// editor that runs `mdpreviewer` from a keybinding shows what a command wrote,
/// and a refusal with no explanation looks like a broken binding.
fn fail(message: &str) -> ! {
    eprintln!("mdpreviewer: {message}");
    process::exit(1);
}

/// Report success. Only when stdout is a real terminal: this is mostly the
/// URL, and an editor that runs `mdpreviewer` from a keybinding captures output
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
            eprintln!("mdpreviewer: cannot start server: {err}");
            process::exit(1);
        }
    };
    server::serve(server, file, config);
}

#[cfg(test)]
mod tests {
    use super::{Args, EarlyExit, Mode, early_exit, expand_home, page_url, parse_args};
    use std::ffi::OsStr;
    use std::path::PathBuf;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    fn early(args: &[&str]) -> Option<EarlyExit> {
        let raw: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        early_exit(&raw)
    }

    #[test]
    fn help_and_version_are_recognised_in_both_spellings() {
        assert_eq!(early(&["--help"]), Some(EarlyExit::Help));
        assert_eq!(early(&["-h"]), Some(EarlyExit::Help));
        assert_eq!(early(&["--version"]), Some(EarlyExit::Version));
        assert_eq!(early(&["-V"]), Some(EarlyExit::Version));
        assert_eq!(early(&["notes.md"]), None);
        assert_eq!(early(&[]), None);
    }

    /// Asking what the options are should answer, not complain, which is the
    /// whole reason these are found before the arguments are parsed.
    #[test]
    fn help_wins_over_bad_arguments_and_over_version() {
        assert!(parse(&["--nonsense"]).is_err());
        assert_eq!(early(&["--nonsense", "--help"]), Some(EarlyExit::Help));
        assert_eq!(early(&["--line"]), None);
        assert_eq!(early(&["--version", "--help"]), Some(EarlyExit::Help));
        assert_eq!(early(&["--help", "--version"]), Some(EarlyExit::Help));
    }

    #[test]
    fn the_version_message_names_the_crate_and_its_version() {
        let message = EarlyExit::Version.message();
        assert!(message.starts_with("mdpreviewer "), "{message}");
        assert!(
            message.trim_end().ends_with(env!("CARGO_PKG_VERSION")),
            "{message}"
        );
    }

    #[test]
    fn the_help_message_shows_every_option_the_parser_accepts() {
        let message = EarlyExit::Help.message();
        for option in [
            "--line",
            "--no-open",
            "--sync",
            "--restart",
            "--quit",
            "--help",
            "--version",
        ] {
            assert!(
                message.contains(option),
                "{option} is missing from:\n{message}"
            );
        }
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
    fn a_leading_tilde_becomes_the_home_directory() {
        let home = Some(OsStr::new("/home/me"));
        let expand = |file| expand_home(file, home);

        assert_eq!(expand("~/notes/a.md"), PathBuf::from("/home/me/notes/a.md"));
        assert_eq!(expand("~"), PathBuf::from("/home/me"));
        // Only a leading `~` path segment counts.
        assert_eq!(expand("~other/a.md"), PathBuf::from("~other/a.md"));
        assert_eq!(expand("notes/~/a.md"), PathBuf::from("notes/~/a.md"));
        assert_eq!(expand("/abs/a.md"), PathBuf::from("/abs/a.md"));
        assert_eq!(expand("a.md"), PathBuf::from("a.md"));
    }

    #[test]
    fn a_tilde_stays_put_without_a_home() {
        assert_eq!(expand_home("~/a.md", None), PathBuf::from("~/a.md"));
        assert_eq!(
            expand_home("~/a.md", Some(OsStr::new(""))),
            PathBuf::from("~/a.md")
        );
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
