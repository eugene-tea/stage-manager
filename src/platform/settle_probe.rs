//! `stage-manager probe-spaces settle …` (round sm-measure): how long a switch takes on
//! screen, read from the window server. Reads only: no window is moved.
//!
//! The windows of the stage coming in are each given with the frame they end at, the windows
//! of the stage going out with the frame they leave from. Their descriptions
//! (`CGWindowListCreateDescriptionFromArray`, these windows only) are read every `--every` ms
//! (default 1); after a few samples the command after `--then` is started (a `ctl` click, a
//! Dock press), and every time below is in ms from that start:
//! - **arrived**: an incoming window on screen at its frame (2 pt) from this sample to the
//!   end of the span; `last_in_ms` is the last of them (the switch's end on screen);
//! - **gone**: an outgoing window no longer on screen at its frame from this sample to the end
//!   (parked in the corner, or in a stage Space off screen); `last_out_ms` the last of them.
//!
//! - **overlay** (with `--overlay PID`, round sm-landing): the first and the last sample with a
//!   large window of that process on screen (the app's motion overlay and its side panels,
//!   [`is_overlay`]): when the switch's pictures were up. An incoming window at its frame well
//!   before the unveil (the overlay's first sample, plus the transition's `motion_ms` and
//!   `unveil_ms`) was in place while its stage's pictures still flew: the suite's `landing`
//!   phase fails on it.
//!
//! `settle --in "ID,X,Y,W,H;…" [--out "ID,X,Y,W,H;…"] [--for MS] [--every MS] [--overlay PID]
//! --then CMD ARG…`: one JSON object on stdout, with the command's own reply (its last line)
//! as `reply`.

use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::windows;
use crate::engine::geometry::Rect;

/// How close (points, each of x, y, w, h) a listed frame must be to count as the frame.
const AT_FRAME: f64 = 2.0;

/// Samples taken before the command starts: the windows' state before the switch.
const WARM_SAMPLES: u32 = 10;

/// One window and the frame it ends at (incoming) or leaves from (outgoing).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spec {
    id: u32,
    frame: Rect,
}

/// `ID,X,Y,W,H;ID,X,Y,W,H` (empty parts skipped).
fn parse_specs(text: &str) -> Result<Vec<Spec>, String> {
    text.split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|part| {
            let v: Vec<&str> = part.split(',').map(str::trim).collect();
            let num = |s: &str| s.parse::<f64>().map_err(|_| format!("not a number in {part:?}"));
            match v.as_slice() {
                [id, x, y, w, h] => Ok(Spec {
                    id: id.parse().map_err(|_| format!("not a window id in {part:?}"))?,
                    frame: Rect { x: num(x)?, y: num(y)?, w: num(w)?, h: num(h)? },
                }),
                _ => Err(format!("want ID,X,Y,W,H, got {part:?}")),
            }
        })
        .collect()
}

/// True when `w`, a window of the watched process, is one of its motion overlay's panels: on
/// screen, at the normal level or above, as large as a display (the strip is a narrow column).
fn is_overlay(w: &windows::CgWindow, pid: i32) -> bool {
    w.pid == pid && w.onscreen && w.layer >= 0 && w.bounds.w >= 800.0 && w.bounds.h >= 500.0
}

fn at_frame(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= AT_FRAME && (a.y - b.y).abs() <= AT_FRAME && (a.w - b.w).abs() <= AT_FRAME && (a.h - b.h).abs() <= AT_FRAME
}

/// One window's run of samples: when it was first at its frame, and since when its state
/// (at the frame or not) has held to the latest sample.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Track {
    first_at: Option<f64>,
    ever_at: bool,
    /// The latest sample's state.
    at: Option<bool>,
    /// The time of the first sample of the run the state is in now.
    since: f64,
}

impl Track {
    fn sample(&mut self, t: f64, at: bool) {
        if at {
            self.ever_at = true;
            if t >= 0.0 {
                self.first_at.get_or_insert(t);
            }
        }
        if self.at != Some(at) {
            self.at = Some(at);
            self.since = t;
        }
    }

    /// Incoming: at its frame for good since this time (0 when it already was before the
    /// command), or None when it is not at its frame at the end.
    fn arrived(&self) -> Option<f64> {
        (self.at == Some(true)).then_some(self.since.max(0.0))
    }

    /// Outgoing: off its frame for good since this time, or None when it is still at its
    /// frame at the end (or never was, and so cannot be said to have gone).
    fn gone(&self) -> Option<f64> {
        (self.at == Some(false) && self.ever_at).then_some(self.since.max(0.0))
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn opt(v: Option<f64>) -> Value {
    v.map_or(Value::Null, |v| json!(round1(v)))
}

/// The latest of `times`, None when any is None (a window that never settled) or none given.
fn last_of(times: &[Option<f64>]) -> Option<f64> {
    if times.is_empty() {
        return None;
    }
    times.iter().try_fold(f64::MIN, |acc, t| t.map(|t| acc.max(t)))
}

struct Opts {
    incoming: Vec<Spec>,
    outgoing: Vec<Spec>,
    span: Duration,
    every: Duration,
    then: Vec<String>,
    /// The app whose motion overlay is watched (`--overlay PID`).
    overlay: Option<i32>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { incoming: Vec::new(), outgoing: Vec::new(), span: Duration::from_millis(1500), every: Duration::from_millis(1), then: Vec::new(), overlay: None };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--in" => o.incoming = parse_specs(&value()?)?,
            "--out" => o.outgoing = parse_specs(&value()?)?,
            "--for" => o.span = Duration::from_millis(value()?.parse().map_err(|_| "--for takes ms".to_string())?),
            "--every" => o.every = Duration::from_millis(value()?.parse::<u64>().map_err(|_| "--every takes ms".to_string())?.max(1)),
            "--overlay" => o.overlay = Some(value()?.parse().map_err(|_| "--overlay takes a process id".to_string())?),
            "--then" => {
                o.then = it.by_ref().cloned().collect();
                break;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if o.incoming.is_empty() {
        return Err("--in names no window".into());
    }
    if o.then.is_empty() {
        return Err("--then names no command".into());
    }
    Ok(o)
}

/// Runs `probe-spaces settle` (see the module docs).
pub fn run(args: &[String]) -> ExitCode {
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            println!("{}", json!({ "ok": false, "error": format!("{e}; usage: probe-spaces settle --in \"ID,X,Y,W,H;…\" [--out …] [--for MS] [--every MS] --then CMD ARG…") }));
            return ExitCode::from(2);
        }
    };
    let ids: Vec<u32> = o.incoming.iter().chain(&o.outgoing).map(|s| s.id).collect();
    let mut tracks = vec![Track::default(); ids.len()];
    let specs: Vec<Spec> = o.incoming.iter().chain(&o.outgoing).copied().collect();
    let start = Instant::now();
    let mut t0: Option<Instant> = None;
    let mut child = None;
    let (mut samples, mut prev, mut max_gap, mut due) = (0u32, start, Duration::ZERO, start);
    let mut cmd_ms = None;
    let mut reply = String::new();
    // The overlay's first and last sample on screen after the command started.
    let (mut overlay_first, mut overlay_last): (Option<f64>, Option<f64>) = (None, None);
    loop {
        let listed = windows::describe(&ids);
        let at = Instant::now();
        let t = t0.map_or(-1.0, |t0| at.duration_since(t0).as_secs_f64() * 1e3);
        for (spec, track) in specs.iter().zip(tracks.iter_mut()) {
            let on = listed.iter().any(|w| w.id == spec.id && w.onscreen && at_frame(w.bounds, spec.frame));
            track.sample(t, on);
        }
        if let Some(pid) = o.overlay.filter(|_| t >= 0.0)
            && windows::on_screen().iter().any(|w| is_overlay(w, pid))
        {
            overlay_first.get_or_insert(t);
            overlay_last = Some(t);
        }
        samples += 1;
        if t0.is_some() {
            max_gap = max_gap.max(at.duration_since(prev));
        }
        prev = at;
        if t0.is_none() && samples >= WARM_SAMPLES {
            let mut cmd = Command::new(&o.then[0]);
            cmd.args(&o.then[1..]).stdout(Stdio::piped()).stderr(Stdio::null());
            t0 = Some(Instant::now());
            match cmd.spawn() {
                Ok(c) => child = Some(c),
                Err(e) => {
                    println!("{}", json!({ "ok": false, "error": format!("the command did not start: {e}") }));
                    return ExitCode::FAILURE;
                }
            }
        }
        if let (Some(t0), Some(c)) = (t0, child.as_mut())
            && cmd_ms.is_none()
            && matches!(c.try_wait(), Ok(Some(_)))
        {
            cmd_ms = Some(t0.elapsed().as_secs_f64() * 1e3);
        }
        if t0.is_some_and(|t0| t0.elapsed() >= o.span) {
            break;
        }
        due += o.every;
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        } else {
            due = now;
        }
    }
    if let Some(c) = child.take()
        && let Ok(out) = c.wait_with_output()
    {
        reply = String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or_default().to_string();
    }
    let n_in = o.incoming.len();
    let (inc, out) = tracks.split_at(n_in);
    let arrived: Vec<Option<f64>> = inc.iter().map(Track::arrived).collect();
    let gone: Vec<Option<f64>> = out.iter().map(Track::gone).collect();
    let span_ms = t0.map_or(0.0, |t0| t0.elapsed().as_secs_f64() * 1e3);
    let reply: Value = serde_json::from_str(&reply).unwrap_or(Value::String(reply));
    println!(
        "{}",
        json!({
            "ok": true,
            "samples": samples,
            "span_ms": span_ms.round(),
            "mean_interval_ms": (span_ms / f64::from(samples.saturating_sub(WARM_SAMPLES).max(1)) * 1000.0).round() / 1000.0,
            "max_gap_ms": round1(max_gap.as_secs_f64() * 1e3),
            "cmd_ms": opt(cmd_ms),
            "incoming": o.incoming.iter().zip(inc).map(|(s, t)| json!({ "id": s.id, "first_at_ms": opt(t.first_at), "arrived_ms": opt(t.arrived()) })).collect::<Vec<_>>(),
            "outgoing": o.outgoing.iter().zip(out).map(|(s, t)| json!({ "id": s.id, "gone_ms": opt(t.gone()) })).collect::<Vec<_>>(),
            "last_in_ms": opt(last_of(&arrived)),
            "last_out_ms": opt(last_of(&gone)),
            "overlay_first_ms": opt(overlay_first),
            "overlay_last_ms": opt(overlay_last),
            "reply": reply,
        })
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_and_refuse() {
        let s = parse_specs("12,10,20,300,200; 13,-5.5,0,1,2;").unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].id, 12);
        assert_eq!(s[1].frame, Rect { x: -5.5, y: 0.0, w: 1.0, h: 2.0 });
        assert!(parse_specs("12,10,20,300").is_err());
        assert!(parse_specs("x,1,2,3,4").is_err());
        assert!(parse_specs("").unwrap().is_empty());
    }

    #[test]
    fn incoming_arrives_at_the_start_of_its_last_run_at_the_frame() {
        let mut t = Track::default();
        for (time, at) in [(-1.0, false), (0.0, false), (5.0, true), (6.0, false), (9.0, true), (12.0, true)] {
            t.sample(time, at);
        }
        assert_eq!(t.arrived(), Some(9.0));
        assert_eq!(t.first_at, Some(5.0));
        t.sample(13.0, false);
        assert_eq!(t.arrived(), None);
    }

    #[test]
    fn a_window_already_there_arrived_at_zero() {
        let mut t = Track::default();
        for (time, at) in [(-1.0, true), (0.0, true), (4.0, true)] {
            t.sample(time, at);
        }
        assert_eq!(t.arrived(), Some(0.0));
    }

    #[test]
    fn outgoing_gone_after_its_last_sample_at_the_frame() {
        let mut t = Track::default();
        for (time, at) in [(-1.0, true), (3.0, true), (7.0, false), (8.0, true), (11.0, false), (20.0, false)] {
            t.sample(time, at);
        }
        assert_eq!(t.gone(), Some(11.0));
        let mut never = Track::default();
        never.sample(1.0, false);
        assert_eq!(never.gone(), None);
    }

    #[test]
    fn last_of_needs_every_window() {
        assert_eq!(last_of(&[Some(3.0), Some(9.0)]), Some(9.0));
        assert_eq!(last_of(&[Some(3.0), None]), None);
        assert_eq!(last_of(&[]), None);
    }

    #[test]
    fn options_need_windows_and_a_command() {
        let args = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(parse(&args("--in 1,0,0,10,10 --for 900 --then ctl debug-click 5 5")).is_ok_and(|o| o.then.len() == 4 && o.span.as_millis() == 900));
        assert!(parse(&args("--in 1,0,0,10,10")).is_err());
        assert!(parse(&args("--for 900 --then ctl")).is_err());
        assert!(parse(&args("--in 1,0,0,10,10 --overlay 321 --then ctl state")).is_ok_and(|o| o.overlay == Some(321)));
        assert!(parse(&args("--in 1,0,0,10,10 --overlay x --then ctl state")).is_err());
    }

    /// The overlay is the watched app's display-sized panel at the normal level or above;
    /// its strip, another app's window, one off screen are not.
    #[test]
    fn the_overlay_is_the_apps_large_panel() {
        let w = |pid: i32, layer: i64, w: f64, onscreen: bool| windows::CgWindow { id: 1, pid, owner: String::new(), name: None, layer, bounds: Rect { x: 0.0, y: 0.0, w, h: 1440.0 }, onscreen, alpha: 1.0 };
        assert!(is_overlay(&w(5, 2, 2560.0, true), 5));
        assert!(is_overlay(&w(5, 0, 2560.0, true), 5), "a side panel under a window, at the normal level");
        assert!(!is_overlay(&w(5, -1, 260.0, true), 5), "the strip");
        assert!(!is_overlay(&w(6, 2, 2560.0, true), 5));
        assert!(!is_overlay(&w(5, 2, 2560.0, false), 5));
    }
}
