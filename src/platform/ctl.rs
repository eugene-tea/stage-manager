//! The ctl socket: a Unix socket at `$TMPDIR/stage-manager.sock` taking one command per
//! line and answering each with one line of JSON. `stage-manager ctl <words...>` is the
//! client.
//!
//! Commands now: `ping`, `dump`, `quit`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::paths;

/// A parsed ctl command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Liveness check; answers with the pid and version.
    Ping,
    /// Observed windows and apps.
    Dump,
    /// Quit the app cleanly.
    Quit,
}

impl Command {
    /// Parses one request line (words separated by whitespace).
    pub fn parse(line: &str) -> Result<Self, String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["ping"] => Ok(Self::Ping),
            ["dump"] => Ok(Self::Dump),
            ["quit"] => Ok(Self::Quit),
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
        eprintln!("usage: stage-manager ctl <ping|dump|quit>");
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
        assert!(Command::parse("").is_err());
        assert!(Command::parse("switch 2").unwrap_err().contains("unknown command"));
    }

    #[test]
    fn error_reply_has_ok_false() {
        let v = error_reply("nope");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "nope");
    }
}
