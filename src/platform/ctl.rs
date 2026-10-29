//! The ctl socket: a Unix socket (see [`paths::socket_path`]) taking one command per line
//! and answering each with one line of JSON. `stage-manager ctl <words...>` is the client.
//!
//! Commands: `ping`, `adopted ID…` (which of these windows the engine manages; the
//! guardian's question), `take-watch OFFER` (an old guardian hands over the windows it waits
//! for, round 8N: [`crate::engine::handoff`]), `dump`, `state`, `quit`, `switch N`, `send N [ID]`, `new [ID]`,
//! `pin [ID]`, `merge N`, `reveal`, `unpark-all`, `reload`, `mark [NOTE…]` (a problem mark
//! in the day's log; `stage-manager mark [NOTE…]` is its client, [`run_mark`]). Without an `ID`, `send`, `new`
//! and `pin` act on the focused window. With `STAGE_MANAGER_TEST=1` in the app's
//! environment, `debug-panic` panics on the main thread (a safety drill), `debug-click X Y
//! [shift]` clicks at a global point, `debug-drag X1 Y1 X2 Y2 [X Y ...] [shift] [hold=MS]` drags
//! along the points (both only where our own windows or a `STAGE_MANAGER_ONLY` app's windows are
//! topmost), `debug-desktop X Y [double | drag X2 Y2]` clicks, double-clicks or drags on the
//! desktop itself (only where nothing but the desktop lies: a fixture's icon under the corner
//! patch), `debug-dock-click BUNDLE` clicks a fixture's Dock item as the Dock-click
//! prediction sees it, `debug-frame ID X Y W H [only]` sets a shown managed window's frame (`only`:
//! nothing else, on any Desktop), `debug-space
//! left|right [numpad]` types the Desktop switch shortcut and reports the Space change, and
//! `debug-fullscreen ID on|off` puts a `STAGE_MANAGER_ONLY` app's window in or out of full
//! screen, `debug-close ID` closes one (its close button), `debug-expose
//! mission-control|app-windows|show-desktop|escape` types Mission Control's own shortcut (or
//! Escape, to leave it), `debug-strip` tells what the strip draws this frame (cards in slot
//! order, each card's front thumbnail and icons where they are drawn, the lifted card) and
//! `debug-overlay [X Y W H]` where the motion overlay's opaque pixels are (within a region),
//! read back from a capture of our own overlay window, with a region's mean colour there and
//! in the corner patch's picture of the wallpaper and the desktop's still (`mean`,
//! `wallpaper`, `still`: a switch's cover checked against the wallpaper). `space` (any
//! time) is the Space shown as the engine sees it.
//!
//! **Who may ask** ([`Peer`]): the socket is created owner-only (`0600`, under a umask, so
//! never wider even for an instant) in a folder only the user can enter. Each connection's
//! peer is checked before any command is served: another account is refused; every command
//! needs the app's own code, judged by the peer's audit token (`LOCAL_PEERTOKEN`: code
//! signed as the app is, [`super::code_sign`]; never by its pid or path, which a process that
//! exits can hand to another), as the `ctl` and `mark` clients, the guardian and the scripted
//! checks are when they run the app's binary; any other process of the user
//! gets only `ping` and `settings` (a second copy's launch check and hand-off), and a crashed
//! copy's guardian `adopted` and `take-watch` (its binary may no longer be signed like this
//! one: an update replaced it). That keeps a
//! script or app without Accessibility from borrowing the app's; it is no wall against a
//! process of the same user, which can run this binary as a client itself. Lines longer than
//! [`MAX_LINE`] end the connection.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::json;

use super::logging::{self, Process};
use super::paths;
use super::space_keys::{Direction, Expose};
use crate::engine::geometry::{Point, Rect};
use crate::engine::handoff::{self, Offer, Trust};

/// A parsed ctl command.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Liveness check; answers with the pid and version.
    Ping,
    /// Which of these window ids the engine manages (the guardian of a crashed copy asks
    /// before it lets a window go): `{"ok":true,"adopted":[...]}`.
    Adopted(Vec<u32>),
    /// An old guardian hands over the windows it still waits for (round 8N): the app takes
    /// them into its safety registry (`{"ok":true,"taken":[...],"pid":N}`).
    TakeWatch(Offer),
    /// The Space shown as the engine sees it: its synthetic id, full screen, a change still
    /// settling, and how many Space changes were handled so far.
    Space,
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
    /// Show the Settings window (a second launch hands off with this).
    Settings,
    /// Write a problem mark into the day's log: the note (may be empty), a state dump and
    /// the last seconds of detailed lines.
    Mark(String),
    /// Panic on the main thread (safety drill; needs `STAGE_MANAGER_TEST=1`).
    DebugPanic,
    /// Test only: quit the way the menu's Quit item does (from inside a menu action).
    DebugMenuQuit,
    /// Test builds with the `update-test` feature: check for an update and install it as
    /// "Install and Relaunch" does, with no alert (live checks of the updater).
    DebugUpdate,
    /// A synthetic pointer move to a global point, resting there for `ms` (the strip's
    /// left-edge reveal), only where our own or a `STAGE_MANAGER_ONLY` app's window is topmost
    /// (needs `STAGE_MANAGER_TEST=1`). The next synthetic click or drag puts the pointer back
    /// where it was before the hover.
    DebugHover {
        /// Global x (top-left origin).
        x: f64,
        /// Global y.
        y: f64,
        /// How long to rest there, in milliseconds.
        ms: u64,
    },
    /// A synthetic left-button drag through global points, posted only when every point is
    /// over our own windows or a `STAGE_MANAGER_ONLY` app's (needs `STAGE_MANAGER_TEST=1`).
    DebugDrag {
        /// Waypoints, global top-left coordinates.
        points: Vec<Point>,
        /// Hold shift.
        shift: bool,
        /// How long the button stays down at the last point (`hold=MS`; default 400 ms).
        hold_ms: u64,
    },
    /// Sets a shown managed window's frame through AX and brings it to the front (needs
    /// `STAGE_MANAGER_TEST=1`). With `only`: the frame and nothing else (no raise, no
    /// focus), for a window shown in its stage on any Desktop, the one shown or not (the
    /// Spaces check's probe of moves on a Desktop not shown).
    DebugFrame {
        /// The window id.
        id: u32,
        /// The frame, global top-left coordinates.
        frame: Rect,
        /// The frame only, on any Desktop.
        only: bool,
    },
    /// Types the Desktop switch shortcut ("Move left/right a space") at the HID tap and
    /// waits for the Space change (needs `STAGE_MANAGER_TEST=1`).
    DebugSpace {
        /// Which neighbour.
        direction: Direction,
        /// Add the numeric-pad flag a hardware arrow key carries.
        numpad: bool,
    },
    /// Puts a `STAGE_MANAGER_ONLY` app's window in or out of full screen through
    /// Accessibility (`AXFullScreen`); any other window is refused (needs
    /// `STAGE_MANAGER_TEST=1`).
    DebugFullscreen {
        /// The window id.
        id: u32,
        /// In (true) or out (false).
        on: bool,
    },
    /// What the strip draws this frame (needs `STAGE_MANAGER_TEST=1`).
    DebugStrip,
    /// Where the motion overlay draws, read back from its pixels, within a global region
    /// when given (needs `STAGE_MANAGER_TEST=1`).
    DebugOverlay {
        /// The region, global top-left coordinates.
        region: Option<Rect>,
    },
    /// Finds the Dock item of a `STAGE_MANAGER_ONLY` app, feeds it to the controller as a
    /// click on it (the Dock-click prediction runs as for a real click), then presses it
    /// (`AXPress`) (needs `STAGE_MANAGER_TEST=1`).
    DebugDockClick {
        /// The app's bundle id.
        bundle: String,
    },
    /// Presses the close button of a `STAGE_MANAGER_ONLY` app's window (a test's own Finder
    /// window); any other window is refused (needs `STAGE_MANAGER_TEST=1`).
    DebugClose {
        /// The window id.
        id: u32,
    },
    /// Types one of Mission Control's shortcuts ("Mission Control", "Application windows",
    /// "Show Desktop") as the user's settings have it, or Escape (`None`) to leave Mission
    /// Control or App Exposé, at the HID tap (needs `STAGE_MANAGER_TEST=1`). Never changes the
    /// Desktop shown.
    DebugExpose {
        /// Which shortcut; `None` is Escape.
        which: Option<Expose>,
    },
    /// A synthetic click at a global point, posted only when our own window or a
    /// `STAGE_MANAGER_ONLY` app's window is the topmost window there (needs
    /// `STAGE_MANAGER_TEST=1`).
    DebugClick {
        /// Global x (top-left origin).
        x: f64,
        /// Global y.
        y: f64,
        /// Hold shift.
        shift: bool,
    },
    /// A synthetic click, double click (`double`) or drag (to `to`) on the desktop itself, posted
    /// only where nothing but the desktop lies along its path (Finder's icons, the wallpaper; our
    /// click-through corner patch seen through; needs `STAGE_MANAGER_TEST=1`): the live check
    /// that a desktop icon under the corner patch still opens and drags.
    DebugDesktop {
        /// Where it is pressed (global, top-left origin).
        at: Point,
        /// A double click.
        double: bool,
        /// Dragged to here instead of clicked.
        to: Option<Point>,
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
            ["space"] => Ok(Self::Space),
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
            ["settings"] => Ok(Self::Settings),
            ["mark", note @ ..] => Ok(Self::Mark(note.join(" "))),
            ["debug-panic"] => Ok(Self::DebugPanic),
            ["debug-menu-quit"] => Ok(Self::DebugMenuQuit),
            ["debug-update"] => Ok(Self::DebugUpdate),
            ["debug-drag", rest @ ..] => {
                let (rest, hold_ms) = match rest.split_last() {
                    Some((last, r)) if last.starts_with("hold=") => {
                        let ms = last["hold=".len()..].parse::<u64>().map_err(|_| format!("expected hold=MS, got {last:?}"))?;
                        (r, ms)
                    }
                    _ => (rest, 400),
                };
                let (coords, shift) = match rest.split_last() {
                    Some((&"shift", c)) => (c, true),
                    _ => (rest, false),
                };
                if coords.len() < 4 || coords.len() % 2 != 0 {
                    return Err("debug-drag needs X1 Y1 X2 Y2 [X Y ...] [shift]".into());
                }
                let nums = coords
                    .iter()
                    .map(|w| w.parse::<f64>().map_err(|_| format!("expected a coordinate, got {w:?}")))
                    .collect::<Result<Vec<_>, _>>()?;
                let points = nums.chunks(2).map(|c| Point::new(c[0], c[1])).collect();
                Ok(Self::DebugDrag { points, shift, hold_ms })
            }
            ["debug-frame", id, x, y, w, h, rest @ ..] if rest.is_empty() || rest == ["only"] => {
                let id = id.parse::<u32>().map_err(|_| format!("expected a window id, got {id:?}"))?;
                let n = |w: &str| w.parse::<f64>().map_err(|_| format!("expected a number, got {w:?}"));
                Ok(Self::DebugFrame { id, frame: Rect::new(n(x)?, n(y)?, n(w)?, n(h)?), only: !rest.is_empty() })
            }
            ["debug-hover", x, y, ms] => {
                let coord = |w: &str| w.parse::<f64>().map_err(|_| format!("expected a coordinate, got {w:?}"));
                let ms = ms.parse::<u64>().map_err(|_| format!("expected milliseconds, got {ms:?}"))?;
                Ok(Self::DebugHover { x: coord(x)?, y: coord(y)?, ms })
            }
            ["debug-click", x, y, rest @ ..] if rest.is_empty() || rest == ["shift"] => {
                let coord = |w: &str| w.parse::<f64>().map_err(|_| format!("expected a coordinate, got {w:?}"));
                Ok(Self::DebugClick { x: coord(x)?, y: coord(y)?, shift: !rest.is_empty() })
            }
            ["debug-desktop", x, y, rest @ ..] => {
                let coord = |w: &str| w.parse::<f64>().map_err(|_| format!("expected a coordinate, got {w:?}"));
                let at = Point::new(coord(x)?, coord(y)?);
                match rest {
                    [] => Ok(Self::DebugDesktop { at, double: false, to: None }),
                    ["double"] => Ok(Self::DebugDesktop { at, double: true, to: None }),
                    ["drag", x2, y2] => Ok(Self::DebugDesktop { at, double: false, to: Some(Point::new(coord(x2)?, coord(y2)?)) }),
                    _ => Err("debug-desktop needs X Y [double | drag X2 Y2]".into()),
                }
            }
            ["debug-space", dir, rest @ ..] if rest.is_empty() || rest == ["numpad"] => {
                let direction = Direction::parse(dir).ok_or_else(|| format!("expected left or right, got {dir:?}"))?;
                Ok(Self::DebugSpace { direction, numpad: !rest.is_empty() })
            }
            ["debug-strip"] => Ok(Self::DebugStrip),
            ["debug-overlay"] => Ok(Self::DebugOverlay { region: None }),
            ["debug-overlay", x, y, w, h] => {
                let n = |w: &str| w.parse::<f64>().map_err(|_| format!("expected a number, got {w:?}"));
                Ok(Self::DebugOverlay { region: Some(Rect::new(n(x)?, n(y)?, n(w)?, n(h)?)) })
            }
            ["debug-expose", "escape"] => Ok(Self::DebugExpose { which: None }),
            ["debug-expose", word] => match Expose::parse(word) {
                Some(which) => Ok(Self::DebugExpose { which: Some(which) }),
                None => Err(format!("expected mission-control, app-windows, show-desktop or escape, got {word:?}")),
            },
            ["debug-fullscreen", id, on] => {
                let id = id.parse::<u32>().map_err(|_| format!("expected a window id, got {id:?}"))?;
                let on = match *on {
                    "on" => true,
                    "off" => false,
                    other => return Err(format!("expected on or off, got {other:?}")),
                };
                Ok(Self::DebugFullscreen { id, on })
            }
            ["debug-dock-click", bundle] => Ok(Self::DebugDockClick { bundle: (*bundle).to_string() }),
            ["adopted", ids @ ..] => ids
                .iter()
                .map(|id| id.parse::<u32>().map_err(|_| format!("expected a window id, got {id:?}")))
                .collect::<Result<Vec<u32>, String>>()
                .map(Self::Adopted),
            [handoff::COMMAND, rest @ ..] => Offer::parse(rest).map(Self::TakeWatch),
            ["debug-close", id] => {
                let id = id.parse::<u32>().map_err(|_| format!("expected a window id, got {id:?}"))?;
                Ok(Self::DebugClose { id })
            }
            [] => Err("empty command".into()),
            _ => Err(format!("unknown command: {}", words.join(" "))),
        }
    }
}

/// How long `debug-space` waits for the Space change after typing the shortcut.
pub const SPACE_WAIT: Duration = Duration::from_secs(6);

/// Answers a command from a peer allowed to send it ([`Peer::may`]); runs on a ctl
/// connection thread.
pub type Handler = Arc<dyn Fn(Command, Peer) -> serde_json::Value + Send + Sync>;

/// Error reply in the protocol's shape.
pub fn error_reply(message: &str) -> serde_json::Value {
    json!({ "ok": false, "error": message })
}

/// The longest command line served; a longer one ends the connection.
pub const MAX_LINE: usize = 64 * 1024;

/// Who is on the other end of a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    /// A process of this user running code signed as this app is (this very binary, or
    /// another copy of the app): every command.
    Ours,
    /// Another process of this user: `ping` and `settings` only.
    SameUser,
    /// Another account, or a peer that could not be read: nothing.
    Stranger,
}

impl Peer {
    /// Judges a peer from its user id against ours (`None`: unreadable) and
    /// `signed_like_us`, the code-signing check of its audit token (asked only of a process
    /// of this user; false on any error).
    pub fn judge(peer_uid: Option<u32>, own_uid: u32, signed_like_us: impl FnOnce() -> bool) -> Self {
        match peer_uid {
            Some(uid) if uid == own_uid && signed_like_us() => Self::Ours,
            Some(uid) if uid == own_uid => Self::SameUser,
            _ => Self::Stranger,
        }
    }

    /// Whether this peer may run `cmd`. `ping`, `settings` and `adopted` are open to every
    /// process of the user, so a second copy of the app (another place on disk) still finds
    /// the running one and never becomes a second manager beside it, and a crashed copy's
    /// guardian can hand its windows over.
    pub fn may(self, cmd: &Command) -> bool {
        match self {
            Self::Ours => true,
            // `adopted` and `take-watch` too: a crashed copy's guardian asks them, and its
            // binary may no longer be signed like this one (an update replaced it). `adopted`
            // answers window ids only; from such a peer `take-watch` takes only windows this
            // app sees parked, of apps it manages ([`Self::trust`], `handoff::vet`), and only
            // ever moves one out of the park zone.
            Self::SameUser => matches!(cmd, Command::Ping | Command::Settings | Command::Adopted(_) | Command::TakeWatch(_)),
            Self::Stranger => false,
        }
    }
}

impl Peer {
    /// How far a handoff from this peer is believed: [`Trust::Signed`] only for our own code.
    pub fn trust(self) -> Trust {
        if self == Self::Ours { Trust::Signed } else { Trust::Unsigned }
    }
}

/// The executable of process `pid` (`proc_pidpath`), resolved; `None` when it cannot be read
/// (the process is gone).
fn exe_of(pid: libc::pid_t) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is writable for the length we pass.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    let path = PathBuf::from(OsString::from_vec(buf));
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}

/// This process's executable, read once the way peers' are: named in the refusal reply.
fn own_exe() -> Option<&'static Path> {
    static OWN: OnceLock<Option<PathBuf>> = OnceLock::new();
    // SAFETY: getpid has no preconditions.
    OWN.get_or_init(|| exe_of(unsafe { libc::getpid() })).as_deref()
}

/// The connection's peer: its user (`getpeereid`) and its code signature, read through its
/// audit token (`LOCAL_PEERTOKEN`); its pid (`LOCAL_PEERPID`) and executable only for the
/// log.
fn peer_of(stream: &UnixStream) -> (Peer, Option<libc::pid_t>, Option<PathBuf>) {
    let fd = stream.as_raw_fd();
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: a valid socket and two valid out pointers.
    let uid = (unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } == 0).then_some(uid);
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: a valid socket; `pid` and `len` are valid for the option's size.
    let read = unsafe {
        libc::getsockopt(fd, libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut len)
    } == 0;
    let pid = read.then_some(pid);
    let exe = pid.and_then(exe_of);
    let signed_like_us = || {
        // audit_token_t: eight 32-bit words.
        let mut token = [0u8; 32];
        let mut len = token.len() as libc::socklen_t;
        // SAFETY: a valid socket; `token` and `len` are valid for the option's size.
        let read = unsafe {
            libc::getsockopt(fd, libc::SOL_LOCAL, libc::LOCAL_PEERTOKEN, token.as_mut_ptr().cast(), &mut len)
        } == 0;
        read && len as usize == token.len() && super::code_sign::signed_like_us(&token)
    };
    // SAFETY: getuid has no preconditions and cannot fail.
    let own_uid = unsafe { libc::getuid() };
    (Peer::judge(uid, own_uid, signed_like_us), pid, exe)
}

/// Binds the socket at `path` owner-only: in a private folder (`0700`, so no other account
/// can reach it even before its own mode is set), then set to `0600`. No umask change: the
/// process-wide mask would narrow files other threads create meanwhile (seen in parallel
/// tests).
fn bind_private(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        crate::private_files::ensure_dir(dir)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Binds the socket (replacing a stale one). Fails when another instance answers on the
/// socket. At launch this runs right after the single-instance claim
/// ([`super::instance::claim`]), before anything is managed: a launch that cannot bind it
/// does not manage windows ([`serve`] answers on it later, once the handler exists).
pub fn bind() -> std::io::Result<UnixListener> {
    let path = paths::socket_path();
    if path.exists() {
        if request("ping", Duration::from_millis(500)).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "another instance is running"));
        }
        std::fs::remove_file(&path)?;
    }
    let listener = bind_private(&path)?;
    if !super::code_sign::own_requirement_known() {
        tracing::warn!("ctl: cannot read this app's own code signature; every client gets only ping and settings");
    }
    tracing::info!("ctl socket at {}", path.display());
    Ok(listener)
}

/// Serves the bound socket ([`bind`]) on a background thread.
pub fn serve(listener: UnixListener, handler: Handler) -> std::io::Result<()> {
    std::thread::Builder::new().name("ctl".into()).spawn(move || {
        // A panic in the accept loop or a connection must not take the app down (Fable M2):
        // a connection that panics is dropped, the socket keeps serving.
        let accept = || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let handler = handler.clone();
                        let spawned = std::thread::Builder::new().name("ctl-conn".into()).spawn(move || {
                            let _ = super::safety::guarded("a ctl connection", || serve_connection(stream, &handler));
                        });
                        if let Err(e) = spawned {
                            tracing::warn!("ctl: cannot spawn connection thread: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("ctl accept failed: {e}"),
                }
            }
        };
        while super::safety::guarded("the ctl socket's accept loop", accept).is_none() {
            std::thread::sleep(Duration::from_millis(500));
        }
    })?;
    Ok(())
}

fn serve_connection(stream: UnixStream, handler: &Handler) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let (peer, pid, exe) = peer_of(&stream);
    if peer == Peer::Stranger {
        tracing::warn!(?pid, "ctl: a connection from another account (or one that cannot be read) refused");
        return;
    }
    let Ok(mut writer) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    let mut refused_noted = false;
    loop {
        let line = match read_line(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                let _ = writeln!(writer, "{}", error_reply(&e));
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match Command::parse(&line) {
            Ok(cmd) if !peer.may(&cmd) => {
                if !std::mem::replace(&mut refused_noted, true) {
                    // The program's name only: its folders may name the user.
                    let name = exe.as_deref().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned());
                    tracing::warn!(?pid, program = ?name, "ctl: a command from a program that is not Stage Manager refused");
                }
                error_reply(&refusal(own_exe()))
            }
            Ok(cmd) => {
                tracing::debug!("ctl: {line}");
                handler(cmd, peer)
            }
            Err(e) => error_reply(&e),
        };
        if writeln!(writer, "{reply}").and_then(|_| writer.flush()).is_err() {
            break;
        }
    }
}

/// The reply to a command refused to a program that is not the app: it names the binary
/// that would be served (`own`, this process's), so a script can switch to it. Only ever sent
/// to a process of the same user.
fn refusal(own: Option<&Path>) -> String {
    let use_this = match own {
        Some(path) => format!("run \"{}\"", path.display()),
        None => "run the running app's own binary".to_string(),
    };
    format!("only Stage Manager's own binary may send this command (ping and settings are open): {use_this} ctl …")
}

/// The next line (without its newline); `None` at the end of the stream (or a read error);
/// an error for a line longer than [`MAX_LINE`] or not UTF-8.
fn read_line(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut buf = Vec::new();
    match reader.take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf) {
        Ok(0) | Err(_) => return Ok(None),
        Ok(_) => {}
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() > MAX_LINE {
        return Err(format!("a command line longer than {MAX_LINE} bytes"));
    }
    String::from_utf8(buf).map(Some).map_err(|_| "a command line that is not UTF-8".to_string())
}

/// Removes the socket file (on quit).
pub fn remove_socket() {
    let _ = std::fs::remove_file(paths::socket_path());
}

/// Sends one request line and returns the reply line.
pub fn request(line: &str, timeout: Duration) -> std::io::Result<String> {
    request_at(&paths::socket_path(), line, timeout)
}

/// [`request`] on the socket at `path` (the single-instance claim also asks at an earlier
/// version's place, [`paths::legacy_socket_path`]).
pub fn request_at(path: &std::path::Path, line: &str, timeout: Duration) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(path)?;
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
            "usage: stage-manager ctl <ping|space|dump|state|quit|switch N|send N [ID]|new [ID]|pin [ID]|merge N|reveal|unpark-all|reload|settings|mark [NOTE]>"
        );
        return ExitCode::from(2);
    }
    // A Desktop switch waits for the Space change (up to SPACE_WAIT) before it answers.
    let timeout = if words[0] == "debug-space" { SPACE_WAIT + Duration::from_secs(5) } else { Duration::from_secs(5) };
    match request(&words.join(" "), timeout) {
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

/// How long `mark` waits for the app (it asks the WM thread for two dumps).
const MARK_WAIT: Duration = Duration::from_secs(12);

/// `stage-manager mark [NOTE…]`: asks the running app to write a problem mark (state dump
/// and recent detail included). With the app not running, the mark is written here, without
/// a dump. Prints where it went.
pub fn run_mark(words: &[String]) -> ExitCode {
    logging::init(Process::Tool { name: "mark", echo: false });
    let note = words.join(" ");
    let line = if note.trim().is_empty() { "mark".to_string() } else { format!("mark {note}") };
    match request(&line, MARK_WAIT) {
        Ok(reply) => {
            let v: serde_json::Value = serde_json::from_str(&reply).unwrap_or_default();
            if v["ok"] == true {
                println!("problem marked at {} in {}", v["at"].as_str().unwrap_or("?"), v["file"].as_str().unwrap_or("?"));
                ExitCode::SUCCESS
            } else {
                eprintln!("the app could not mark: {}", v["error"].as_str().unwrap_or(&reply));
                ExitCode::from(1)
            }
        }
        Err(e) => {
            let why = format!("Stage Manager is not reachable ({e}): no state to dump");
            match logging::write_mark(&note, &[("app", why.clone())]) {
                Ok(m) => {
                    println!("{why}; problem marked at {} in {}", m.at, m.file.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{why}; cannot mark: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_commands() {
        assert_eq!(Command::parse("ping"), Ok(Command::Ping));
        assert_eq!(Command::parse("adopted 4 9"), Ok(Command::Adopted(vec![4, 9])));
        assert_eq!(Command::parse("adopted"), Ok(Command::Adopted(vec![])));
        assert!(Command::parse("adopted x").is_err());
        let offer = Offer { app: 1, guardian: 2, corners: vec![], windows: vec![] };
        assert_eq!(Command::parse(&offer.line().expect("line")), Ok(Command::TakeWatch(offer)));
        assert!(Command::parse("take-watch").is_err());
        assert!(Command::parse("take-watch 4 9").is_err());
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
        assert_eq!(Command::parse("settings"), Ok(Command::Settings));
        assert_eq!(Command::parse("mark"), Ok(Command::Mark(String::new())));
        assert_eq!(Command::parse("mark  strip  gone "), Ok(Command::Mark("strip gone".into())));
        assert_eq!(Command::parse("debug-panic"), Ok(Command::DebugPanic));
        assert_eq!(Command::parse("debug-menu-quit"), Ok(Command::DebugMenuQuit));
        assert_eq!(
            Command::parse("debug-click 60 700.5 shift"),
            Ok(Command::DebugClick { x: 60.0, y: 700.5, shift: true })
        );
        assert_eq!(
            Command::parse("debug-desktop 2490 1380 double"),
            Ok(Command::DebugDesktop { at: Point::new(2490.0, 1380.0), double: true, to: None })
        );
        assert_eq!(
            Command::parse("debug-desktop 2490 1380 drag 2200 1380"),
            Ok(Command::DebugDesktop { at: Point::new(2490.0, 1380.0), double: false, to: Some(Point::new(2200.0, 1380.0)) })
        );
        assert!(Command::parse("debug-desktop 2490 1380 triple").is_err());
        assert_eq!(
            Command::parse("debug-desktop 1 2"),
            Ok(Command::DebugDesktop { at: Point::new(1.0, 2.0), double: false, to: None })
        );
        assert!(Command::parse("debug-click 60").is_err());
        assert_eq!(Command::parse("debug-hover 1 720 400"), Ok(Command::DebugHover { x: 1.0, y: 720.0, ms: 400 }));
        assert!(Command::parse("debug-hover 1 720").is_err());
        assert_eq!(
            Command::parse("debug-drag 1 2 3 4 shift"),
            Ok(Command::DebugDrag { points: vec![Point::new(1.0, 2.0), Point::new(3.0, 4.0)], shift: true, hold_ms: 400 })
        );
        assert_eq!(
            Command::parse("debug-drag 1 2 3 4 hold=1500"),
            Ok(Command::DebugDrag { points: vec![Point::new(1.0, 2.0), Point::new(3.0, 4.0)], shift: false, hold_ms: 1500 })
        );
        assert!(Command::parse("debug-drag 1 2 3").is_err());
        assert_eq!(
            Command::parse("debug-frame 7 200 300 800 600"),
            Ok(Command::DebugFrame { id: 7, frame: Rect::new(200.0, 300.0, 800.0, 600.0), only: false })
        );
        assert_eq!(
            Command::parse("debug-frame 7 200 300 800 600 only"),
            Ok(Command::DebugFrame { id: 7, frame: Rect::new(200.0, 300.0, 800.0, 600.0), only: true })
        );
        assert!(Command::parse("debug-frame 7 200 300 800 600 raise").is_err());
        assert!(Command::parse("debug-frame 7 200 300 800").is_err());
        assert_eq!(
            Command::parse("debug-dock-click dev.example.app"),
            Ok(Command::DebugDockClick { bundle: "dev.example.app".into() })
        );
        assert!(Command::parse("debug-dock-click").is_err());
        assert!(Command::parse("debug-click 60 70 alt").is_err());
        assert_eq!(Command::parse("space"), Ok(Command::Space));
        assert_eq!(
            Command::parse("debug-space right"),
            Ok(Command::DebugSpace { direction: Direction::Right, numpad: false })
        );
        assert_eq!(
            Command::parse("debug-space left numpad"),
            Ok(Command::DebugSpace { direction: Direction::Left, numpad: true })
        );
        assert!(Command::parse("debug-space up").is_err());
        assert!(Command::parse("debug-space").is_err());
        assert!(Command::parse("debug-space left fast").is_err());
        assert_eq!(Command::parse("debug-fullscreen 42 on"), Ok(Command::DebugFullscreen { id: 42, on: true }));
        assert_eq!(Command::parse("debug-fullscreen 42 off"), Ok(Command::DebugFullscreen { id: 42, on: false }));
        assert_eq!(Command::parse("debug-close 42"), Ok(Command::DebugClose { id: 42 }));
        assert!(Command::parse("debug-close").is_err());
        assert!(Command::parse("debug-fullscreen 42 toggle").is_err());
        assert!(Command::parse("debug-fullscreen x on").is_err());
        assert!(Command::parse("debug-fullscreen 42").is_err());
        assert_eq!(
            Command::parse("debug-expose mission-control"),
            Ok(Command::DebugExpose { which: Some(Expose::MissionControl) })
        );
        assert_eq!(Command::parse("debug-expose app-windows"), Ok(Command::DebugExpose { which: Some(Expose::AppWindows) }));
        assert_eq!(Command::parse("debug-expose show-desktop"), Ok(Command::DebugExpose { which: Some(Expose::ShowDesktop) }));
        assert_eq!(Command::parse("debug-expose escape"), Ok(Command::DebugExpose { which: None }));
        assert!(Command::parse("debug-expose").is_err());
        assert!(Command::parse("debug-expose launchpad").is_err());
        assert!(Command::parse("").is_err());
        assert!(Command::parse("switch 0").is_err());
        assert!(Command::parse("switch x").is_err());
        assert!(Command::parse("send 2 x").is_err());
        assert!(Command::parse("send 2 3 4").is_err());
        assert!(Command::parse("tile").unwrap_err().contains("unknown command"));
    }

    #[test]
    fn only_our_own_code_gets_more_than_ping_and_settings() {
        let never_asked = || -> bool { panic!("another account is never checked further") };
        assert_eq!(Peer::judge(Some(501), 501, || true), Peer::Ours, "this binary or a copy signed as it is");
        assert_eq!(Peer::judge(Some(501), 501, || false), Peer::SameUser, "signed otherwise, unsigned, or gone");
        assert_eq!(Peer::judge(Some(0), 501, never_asked), Peer::Stranger, "another account, root too");
        assert_eq!(Peer::judge(None, 501, never_asked), Peer::Stranger, "unreadable");
        let offer = Offer { app: 1, guardian: 2, corners: vec![], windows: vec![] };
        for cmd in [Command::Ping, Command::Settings, Command::Adopted(vec![4]), Command::TakeWatch(offer)] {
            assert!(Peer::SameUser.may(&cmd) && Peer::Ours.may(&cmd) && !Peer::Stranger.may(&cmd), "{cmd:?}");
        }
        for cmd in [Command::Dump, Command::State, Command::Send(1, None), Command::Quit, Command::UnparkAll, Command::DebugPanic] {
            assert!(!Peer::SameUser.may(&cmd), "{cmd:?}");
            assert!(Peer::Ours.may(&cmd), "{cmd:?}");
        }
        // A handoff from a program not signed like us is believed only for parked windows
        // (the Codex review of 10-21, R4; `handoff::vet`).
        assert_eq!(Peer::Ours.trust(), Trust::Signed);
        assert_eq!(Peer::SameUser.trust(), Trust::Unsigned);
    }

    #[test]
    fn a_refusal_names_the_binary_that_would_be_served() {
        let ours = Path::new("/Applications/Stage Manager.app/Contents/MacOS/stage-manager");
        let reply = refusal(Some(ours));
        assert!(reply.contains("ping and settings are open"), "{reply}");
        assert!(reply.contains("run \"/Applications/Stage Manager.app/Contents/MacOS/stage-manager\" ctl"), "{reply}");
        assert!(refusal(None).contains("the running app's own binary"));
    }

    #[test]
    fn this_test_binary_is_ours_and_its_own_peer() {
        let (a, b) = UnixStream::pair().expect("pair");
        let (peer, pid, exe) = peer_of(&a);
        assert_eq!(peer, Peer::Ours, "a socket pair's peer is this very process");
        assert_eq!(pid, Some(std::process::id() as libc::pid_t));
        assert_eq!(exe.as_deref(), own_exe());
        drop(b);
    }

    #[test]
    fn a_program_signed_otherwise_is_not_ours() {
        // macOS's own `nc` connects: same user, another path, Apple's signature, which this
        // test binary's requirement (its own ad-hoc code) does not admit.
        let nc = Path::new("/usr/bin/nc");
        if !nc.exists() {
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("p.sock");
        let listener = UnixListener::bind(&path).expect("binds");
        let mut child = std::process::Command::new(nc)
            .arg("-U")
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("nc runs");
        let (stream, _) = listener.accept().expect("accepts");
        let (peer, pid, exe) = peer_of(&stream);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(peer, Peer::SameUser, "{pid:?} {exe:?}");
        assert_eq!(pid, Some(child.id() as libc::pid_t));
    }

    #[test]
    fn long_or_broken_lines_end_the_connection() {
        let mut ok = BufReader::new(&b"ping\nstate"[..]);
        assert_eq!(read_line(&mut ok), Ok(Some("ping".into())));
        assert_eq!(read_line(&mut ok), Ok(Some("state".into())));
        assert_eq!(read_line(&mut ok), Ok(None));
        let exact = "x".repeat(MAX_LINE);
        assert_eq!(read_line(&mut BufReader::new(exact.as_bytes())), Ok(Some(exact.clone())));
        let long = "x".repeat(MAX_LINE + 1);
        assert!(read_line(&mut BufReader::new(long.as_bytes())).is_err());
        assert!(read_line(&mut BufReader::new(&b"\xff\n"[..])).is_err());
    }

    #[test]
    fn the_socket_is_bound_owner_only_in_a_private_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir(&home).expect("mkdir");
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let path = home.join("t.sock");
        let _listener = bind_private(&path).expect("binds");
        assert_eq!(crate::private_files::mode(&path).expect("mode"), 0o600);
        assert_eq!(crate::private_files::mode(&home).expect("mode"), 0o700);
    }

    #[test]
    fn error_reply_has_ok_false() {
        let v = error_reply("nope");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "nope");
    }
}
