//! The ctl socket: a Unix socket (see [`paths::socket_path`]) taking one command per line
//! and answering each with one line of JSON. `stage-manager ctl <words...>` is the client.
//!
//! Commands: `ping`, `dump`, `state`, `quit`, `switch N`, `send N [ID]`, `new [ID]`,
//! `pin [ID]`, `merge N`, `reveal`, `unpark-all`, `reload`. Without an `ID`, `send`, `new`
//! and `pin` act on the focused window. With `STAGE_MANAGER_TEST=1` in the app's
//! environment, `debug-panic` panics on the main thread (a safety drill) and `debug-click X Y
//! [shift]` clicks our own strip at a global point.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::paths;

/// A parsed ctl command.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Liveness check; answers with the pid and version.
    Ping,
    /// Observed windows and apps.
    Dump,
    /// The engine's stages, windows, frames and parked flags.
    State,
    /// Quit the app cleanly (every window unparked first).
    Quit,
    /// Switch to stage N (1-based).
    Switch(usize),
    /// Send a window (the focused one when `None`) to stage N.
    Send(usize, Option<u32>),
    /// Send a window (the focused one when `None`) to a new stage.
    New(Option<u32>),
    /// Pin or unpin a window (the focused one when `None`).
    Pin(Option<u32>),
    /// Move stage N's windows into the active stage (a shift-click on its card).
    Merge(usize),
    /// Reveal the desktop (a wallpaper click).
    Reveal,
    /// Bring every parked window back onscreen (the stages stay).
    UnparkAll,
    /// Re-read config.toml.
    Reload,
    /// Panic on the main thread (safety drill; needs `STAGE_MANAGER_TEST=1`).
    DebugPanic,
    /// A synthetic click at a global point, posted only when our own window (the strip) is
    /// the topmost window there (needs `STAGE_MANAGER_TEST=1`).
    DebugClick {
        /// Global x (top-left origin).
        x: f64,
        /// Global y.
        y: f64,
        /// Hold shift.
        shift: bool,
    },
}

fn number(word: &str) -> Result<usize, String> {
    word.parse::<usize>().ok().filter(|n| *n >= 1).ok_or_else(|| format!("expected a stage number, got {word:?}"))
}

fn window_id(word: Option<&&str>) -> Result<Option<u32>, String> {
    word.map(|w| w.parse::<u32>().map_err(|_| format!("expected a window id, got {w:?}"))).transpose()
}

impl Command {
    /// Parses one request line (words separated by whitespace).
    pub fn parse(line: &str) -> Result<Self, String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["ping"] => Ok(Self::Ping),
            ["dump"] => Ok(Self::Dump),
            ["state"] => Ok(Self::State),
            ["quit"] => Ok(Self::Quit),
            ["switch", n] => Ok(Self::Switch(number(n)?)),
            ["send", n, rest @ ..] if rest.len() <= 1 => Ok(Self::Send(number(n)?, window_id(rest.first())?)),
            ["new", rest @ ..] if rest.len() <= 1 => Ok(Self::New(window_id(rest.first())?)),
            ["pin", rest @ ..] if rest.len() <= 1 => Ok(Self::Pin(window_id(rest.first())?)),
            ["merge", n] => Ok(Self::Merge(number(n)?)),
            ["reveal"] => Ok(Self::Reveal),
            ["unpark-all"] => Ok(Self::UnparkAll),
            ["reload"] => Ok(Self::Reload),
            ["debug-panic"] => Ok(Self::DebugPanic),
            ["debug-click", x, y, rest @ ..] if rest.is_empty() || rest == ["shift"] => {
                let coord = |w: &str| w.parse::<f64>().map_err(|_| format!("expected a coordinate, got {w:?}"));
                Ok(Self::DebugClick { x: coord(x)?, y: coord(y)?, shift: !rest.is_empty() })
            }
            [] => Err("empty command".into()),
            _ => Err(format!("unknown command: {}", words.join(" "))),
        }
    }
}

/// Answers a command; runs on a ctl connection thread.
pub type Handler = Arc<dyn Fn(Command) -> serde_json::Value + Send + Sync>;

/// Error reply in the protocol's shape.
pub fn error_reply(message: &str) -> serde_json::Value {
    json!({ "ok": false, "error": message })
}

/// Binds the socket (replacing a stale one) and serves it on a background thread. Fails
/// when another instance answers on the socket.
pub fn serve(handler: Handler) -> std::io::Result<()> {
    let path = paths::socket_path();
    if path.exists() {
        if request("ping", Duration::from_millis(500)).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "another instance is running"));
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!("ctl socket at {}", path.display());
    std::thread::Builder::new().name("ctl".into()).spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let handler = handler.clone();
                    let spawned =
                        std::thread::Builder::new().name("ctl-conn".into()).spawn(move || serve_connection(stream, &handler));
                    if let Err(e) = spawned {
                        tracing::warn!("ctl: cannot spawn connection thread: {e}");
                    }
                }
                Err(e) => tracing::warn!("ctl accept failed: {e}"),
            }
        }
    })?;
    Ok(())
}

fn serve_connection(stream: UnixStream, handler: &Handler) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let Ok(mut writer) = stream.try_clone() else { return };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match Command::parse(&line) {
            Ok(cmd) => {
                tracing::debug!("ctl: {line}");
                handler(cmd)
            }
            Err(e) => error_reply(&e),
        };
        if writeln!(writer, "{reply}").and_then(|_| writer.flush()).is_err() {
            break;
        }
    }
}

/// Removes the socket file (on quit).
pub fn remove_socket() {
    let _ = std::fs::remove_file(paths::socket_path());
}

/// Sends one request line and returns the reply line.
pub fn request(line: &str, timeout: Duration) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(paths::socket_path())?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    writeln!(stream, "{line}")?;
    stream.flush()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    if reply.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "no reply"));
    }
    Ok(reply.trim_end().to_string())
}

/// `stage-manager ctl <words...>`: prints the reply; exit 1 on an error reply, 2 when the
/// app is not reachable.
pub fn run_client(words: &[String]) -> ExitCode {
    if words.is_empty() {
        eprintln!(
            "usage: stage-manager ctl <ping|dump|state|quit|switch N|send N [ID]|new [ID]|pin [ID]|merge N|reveal|unpark-all|reload>"
        );
        return ExitCode::from(2);
    }
    match request(&words.join(" "), Duration::from_secs(5)) {
        Ok(reply) => {
            println!("{reply}");
            let ok = serde_json::from_str::<serde_json::Value>(&reply)
                .map(|v| v["ok"] == true)
                .unwrap_or(false);
            if ok { ExitCode::SUCCESS } else { ExitCode::from(1) }
        }
        Err(e) => {
            eprintln!("stage-manager is not reachable at {}: {e}", paths::socket_path().display());
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_commands() {
        assert_eq!(Command::parse("ping"), Ok(Command::Ping));
        assert_eq!(Command::parse("  dump \n"), Ok(Command::Dump));
        assert_eq!(Command::parse("quit"), Ok(Command::Quit));
        assert_eq!(Command::parse("state"), Ok(Command::State));
        assert_eq!(Command::parse("switch 2"), Ok(Command::Switch(2)));
        assert_eq!(Command::parse("send 3"), Ok(Command::Send(3, None)));
        assert_eq!(Command::parse("send 3 4711"), Ok(Command::Send(3, Some(4711))));
        assert_eq!(Command::parse("new"), Ok(Command::New(None)));
        assert_eq!(Command::parse("new 12"), Ok(Command::New(Some(12))));
        assert_eq!(Command::parse("pin 12"), Ok(Command::Pin(Some(12))));
        assert_eq!(Command::parse("merge 2"), Ok(Command::Merge(2)));
        assert_eq!(Command::parse("reveal"), Ok(Command::Reveal));
        assert_eq!(Command::parse("unpark-all"), Ok(Command::UnparkAll));
        assert_eq!(Command::parse("reload"), Ok(Command::Reload));
        assert_eq!(Command::parse("debug-panic"), Ok(Command::DebugPanic));
        assert_eq!(
            Command::parse("debug-click 60 700.5 shift"),
            Ok(Command::DebugClick { x: 60.0, y: 700.5, shift: true })
        );
        assert!(Command::parse("debug-click 60").is_err());
        assert!(Command::parse("debug-click 60 70 alt").is_err());
        assert!(Command::parse("").is_err());
        assert!(Command::parse("switch 0").is_err());
        assert!(Command::parse("switch x").is_err());
        assert!(Command::parse("send 2 x").is_err());
        assert!(Command::parse("send 2 3 4").is_err());
        assert!(Command::parse("tile").unwrap_err().contains("unknown command"));
    }

    #[test]
    fn error_reply_has_ok_false() {
        let v = error_reply("nope");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "nope");
    }
}
