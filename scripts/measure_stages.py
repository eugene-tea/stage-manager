#!/usr/bin/env python3
"""What the app costs, measured the same way each time (round sm-measure). Run through
scripts/measure.sh, which builds, signs and runs every engine in turn.

    measure_stages.py run --engine off|corner|spaces --out DIR [--run N] [SCENARIO...]
    measure_stages.py summary DIR

Every scenario appends one JSON line to DIR/measure.jsonl and prints a short table; `summary`
gives the median and the spread (min-max) of every number by scenario and engine. `off` is
the app not running: the same fixture windows, the floor.

Scenarios (all by default, in this order):
- rest: N stages (SM_STAGES, default 4: the main fixture and fixtures A, B, C, 2 windows
  each), nothing touched for SM_REST_S seconds (60) after a SM_SETTLE_S (10) settle. Our
  process (and its guardian): CPU time over wall time and wakeups a second
  (`proc_pid_rusage`), billed energy; `top`'s %CPU, POWER and idle wakeups over the same
  span; the window server's and replayd's CPU (`ps` time over wall time) and POWER.
- sample: the next SM_REST_S seconds, `sample` of our process (1 ms); its active samples
  (a thread not waiting) split by where the work comes from (a run-loop timer and the
  function it calls, an Accessibility notice, the main queue, Core Animation's commit, a
  thread of ours) and by what it does (capture, drawing, Accessibility, window list, other).
  The raw file stays in DIR. Not with `off`.
- switch: SM_SWITCHES (20) card clicks and as many Dock clicks (`ctl debug-click` on a card,
  `ctl debug-dock-click`), timed from the command's start to the last incoming window at
  its frame for good, read from the window server every ms (`probe-spaces settle`); then the
  same number of each unsampled, for our CPU, the window server's and replayd's over
  SM_SWITCH_CPU_S (1.5) seconds from the command. Not with `off`.
- call: a call stand-in (the fixture's --frame-clock: a floating window drawing on every
  display refresh, counting the refreshes it missed; its own bundle ID, never managed) in
  front, SM_REST_S seconds at rest, then SM_SWITCHES activations of the other stages' apps
  (as a Dock click activates; with `off`, macOS only raises them), 2.5 s apart.
- launch: Alpha's stage in front, Charlie opened SM_LAUNCHES (10) times while
  `probe-spaces overlap` reads the on-screen list every ms (the live suite's launch phase):
  how long Charlie's window shows over Alpha's stage. Not with `off`.

The app runs from the dev copy with captures on (as installed), STAGE_MANAGER_ONLY the
fixtures, in a scratch home; fixture windows only, at frames clear of the strip.
"""

import ctypes
import json
import os
import re
import statistics
import subprocess
import sys
import time

SCENARIOS = ["rest", "sample", "switch", "call", "launch"]


def parse_args(argv):
    if len(argv) >= 2 and argv[0] == "summary":
        return {"cmd": "summary", "out": argv[1]}
    if not argv or argv[0] != "run":
        sys.exit(__doc__)
    o = {"cmd": "run", "engine": None, "out": None, "run": 1, "scenarios": []}
    it = iter(argv[1:])
    for a in it:
        if a == "--engine":
            o["engine"] = next(it)
        elif a == "--out":
            o["out"] = next(it)
        elif a == "--run":
            o["run"] = int(next(it))
        elif a in SCENARIOS:
            o["scenarios"].append(a)
        else:
            sys.exit(f"unknown argument {a}\n{__doc__}")
    if o["engine"] not in ("off", "corner", "spaces") or not o["out"]:
        sys.exit(__doc__)
    o["scenarios"] = o["scenarios"] or list(SCENARIOS)
    return o


ARGS = parse_args(sys.argv[1:])
if ARGS["cmd"] == "run":
    # test_stages reads the engine once, at import.
    os.environ["SM_ENGINE"] = "corner" if ARGS["engine"] == "off" else ARGS["engine"]
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import test_stages as T  # noqa: E402

STAGES = int(os.environ.get("SM_STAGES") or 4)
SETTLE_S = float(os.environ.get("SM_SETTLE_S") or 10)
REST_S = float(os.environ.get("SM_REST_S") or 60)
SWITCHES = int(os.environ.get("SM_SWITCHES") or 20)
SWITCH_CPU_S = float(os.environ.get("SM_SWITCH_CPU_S") or 1.5)
LAUNCHES = int(os.environ.get("SM_LAUNCHES") or 10)
# Frames from the visible frame's top-left, clear of the strip's column (its cards stay
# clickable, never covered).
FRAMES = "330,60,1000,640;560,220,1000,640"


# ----- counters --------------------------------------------------------------------------

class RUsage(ctypes.Structure):
    """struct rusage_info_v4 (sys/resource.h)."""
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(n, ctypes.c_uint64) for n in (
        "user", "system", "pkg_idle_wkups", "interrupt_wkups", "pageins", "wired_size", "resident_size",
        "phys_footprint", "proc_start_abstime", "proc_exit_abstime", "child_user", "child_system",
        "child_pkg_idle_wkups", "child_interrupt_wkups", "child_pageins", "child_elapsed_abstime",
        "diskio_bytesread", "diskio_byteswritten", "cpu_time_qos_default", "cpu_time_qos_maintenance",
        "cpu_time_qos_background", "cpu_time_qos_utility", "cpu_time_qos_legacy", "cpu_time_qos_user_initiated",
        "cpu_time_qos_user_interactive", "billed_system_time", "serviced_system_time", "logical_writes",
        "lifetime_max_phys_footprint", "instructions", "cycles", "billed_energy", "serviced_energy",
        "interval_max_phys_footprint", "runnable_time")]


class Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


_LIBPROC = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
_TB = Timebase()
ctypes.CDLL("/usr/lib/libSystem.dylib").mach_timebase_info(ctypes.byref(_TB))
TICK_NS = _TB.numer / _TB.denom  # rusage times are in mach ticks on Apple silicon


def rusage(pid):
    """(cpu s, idle wakeups, interrupt wakeups, billed energy nJ) of one of our processes, or
    None (gone, or not ours to read)."""
    if not pid:
        return None
    r = RUsage()
    if _LIBPROC.proc_pid_rusage(int(pid), 4, ctypes.byref(r)) != 0:
        return None
    return ((r.user + r.system) * TICK_NS / 1e9, r.pkg_idle_wkups, r.interrupt_wkups, r.billed_energy)


def ps_cpu(pid):
    """CPU seconds of any process (`ps` time, 10 ms steps), or None."""
    if not pid:
        return None
    r = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True)
    t = r.stdout.strip()
    if not t:
        return None
    parts = [float(p) for p in t.split(":")]
    secs = 0.0
    for p in parts:
        secs = secs * 60 + p
    return secs


def pid_named(name):
    r = subprocess.run(["pgrep", "-x", name], capture_output=True, text=True)
    p = r.stdout.split()
    return int(p[0]) if p else None


def start_top(pids, secs):
    """`top` over `secs` for these pids (two samples: the second covers the span)."""
    args = ["top", "-l", "2", "-s", str(int(round(secs))), "-stats", "pid,cpu,power,idlew"]
    for p in pids:
        if p:
            args += ["-pid", str(p)]
    return subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)


def top_out(proc):
    """pid -> {cpu, power, idlew} from the last sample of `top`."""
    try:
        out, _ = proc.communicate(timeout=REST_S + 30)
    except subprocess.TimeoutExpired:
        proc.kill()
        return {}
    rows = {}
    for line in out.splitlines():
        f = line.split()
        if len(f) == 4 and f[0].isdigit():
            num = lambda s: float(s.rstrip("+-*") or 0)  # noqa: E731
            rows[int(f[0])] = {"cpu": num(f[1]), "power": num(f[2]), "idlew": num(f[3])}
    return rows


class Span:
    """Counters read at the start and the end of a span: our app and guardian (rusage), the
    window server and replayd (`ps`), optionally `top` over it."""

    def __init__(self, with_top_secs=None):
        self.app = T.app_pid()
        self.guard = (T.guardian_pids(self.app) or [None])[0] if self.app else None
        self.ws = pid_named("WindowServer")
        self.rp0 = pid_named("replayd")
        self.top = start_top([self.app, self.guard, self.ws, self.rp0], with_top_secs) if with_top_secs else None
        self.t0 = time.time()
        self.a0, self.g0 = rusage(self.app), rusage(self.guard)
        self.ws0, self.r0 = ps_cpu(self.ws), ps_cpu(self.rp0)

    def end(self):
        a1, g1 = rusage(self.app), rusage(self.guard)
        ws1 = ps_cpu(self.ws)
        rp1 = pid_named("replayd")
        r1 = ps_cpu(rp1)
        wall = time.time() - self.t0
        out = {"wall_s": round(wall, 3)}

        def d(a, b, i, scale, digits):
            return round((b[i] - a[i]) * scale, digits) if a and b else None

        if self.app:
            out.update({
                "app_cpu_ms": d(self.a0, a1, 0, 1e3, 1),
                "app_cpu_pct": d(self.a0, a1, 0, 100 / wall, 3),
                "app_idle_wakeups_s": d(self.a0, a1, 1, 1 / wall, 1),
                "app_intr_wakeups_s": d(self.a0, a1, 2, 1 / wall, 1),
                "app_energy_mw": d(self.a0, a1, 3, 1 / wall / 1e6, 2),
            })
            out["guardian_cpu_pct"] = d(self.g0, g1, 0, 100 / wall, 3) or 0.0
        if self.ws0 is not None and ws1 is not None:
            out["ws_cpu_ms"] = round((ws1 - self.ws0) * 1e3)
            out["ws_cpu_pct"] = round(100 * (ws1 - self.ws0) / wall, 2)
        # replayd may start or restart within the span: all of a new one's time is in it.
        r = (r1 - self.r0) if (rp1 == self.rp0 and r1 is not None and self.r0 is not None) else (r1 or 0.0)
        out["replayd_cpu_ms"] = round(r * 1e3)
        out["replayd_cpu_pct"] = round(100 * r / wall, 2)
        if self.top:
            rows = top_out(self.top)
            for key, pid in (("app", self.app), ("guardian", self.guard), ("ws", self.ws), ("replayd", self.rp0)):
                if pid and pid in rows:
                    out[f"top_{key}_cpu"] = rows[pid]["cpu"]
                    out[f"top_{key}_power"] = rows[pid]["power"]
                    out[f"top_{key}_idlew"] = rows[pid]["idlew"]
        return out


# ----- set-up ----------------------------------------------------------------------------

OUT = ARGS.get("out")
JSONL = os.path.join(OUT, "measure.jsonl")
ENGINE = ARGS.get("engine")
RUN = ARGS.get("run")
CLOCK_ID = (os.environ.get("SM_FIXTURE_ID") or "dev.eugene.stagemanager.fixture.stages") + ".clock"
CLOCK_EXE = "Clock.app/Contents/MacOS/sm-fixture"


def say(msg):
    print(msg, flush=True)


def record(scenario, data):
    row = {"scenario": scenario, "engine": ENGINE, "run": RUN, "at": time.strftime("%Y-%m-%d %H:%M:%S"), **data}
    with open(JSONL, "a") as f:
        f.write(json.dumps(row) + "\n")
    say(json.dumps(row))
    return row


def apps():
    """The stage apps: (open path, process pattern, bundle ID, title), the main fixture first."""
    out = [(T.FIX_APP, T.FIX_EXE, T.FIX_ID, "Fixture")]
    for k in ("a", "b", "c"):
        bundle, path, pattern, title = T.EXTRA[k]
        out.append((path, pattern, bundle, title))
    return out[:STAGES]


def stop_clock():
    for pid in T.pids_of(CLOCK_EXE):
        os.kill(pid, 15)
    T.wait_until(lambda: not T.pids_of(CLOCK_EXE), 10, "the call stand-in to quit")


def stop_everything():
    T.stop_app()
    T.stop_all_fixtures()
    stop_clock()


def fixtures_up():
    """Every stage app with 2 windows at the strip-clear frames, in the background."""
    for path, pattern, bundle, title in apps():
        subprocess.run(["open", "-n", "-g", path, "--args", "--windows", "2", "--title", title, "--background",
                        "--frames", FRAMES], check=True)
    for path, pattern, bundle, title in apps():
        if not T.wait_until(lambda b=bundle: len(T.fixture_windows([b])[0]) >= 2, 15, f"{title}'s 2 windows"):
            raise RuntimeError(f"{title} did not open its windows")


def fresh_home():
    """No saved stages or config from an earlier scenario: a window would go back to its
    saved stage (a relaunched Charlie to Charlie's stage of the four) instead of a new one."""
    for f in ("state.json", "config.toml", ".config-last-good.toml"):
        p = os.path.join(T.HOME, f)
        if os.path.exists(p):
            os.remove(p)


def front_first_app():
    """The first stage app activated, so its windows are in front of every other app's (the
    fixtures open in the background)."""
    subprocess.run(["open", apps()[0][0]], check=True)
    time.sleep(1.0)


def stages_up():
    """The fixtures, and (unless `off`) the app with one stage per fixture app, the first in
    front. Returns the app's state (None with `off`)."""
    stop_everything()
    fresh_home()
    fixtures_up()
    if ENGINE == "off":
        front_first_app()
        return None
    s = T.start_app(2 * STAGES, extra_env={"STAGE_MANAGER_TEST": "1"}, capture=True)
    if s is None:
        raise RuntimeError("the app did not come up")
    kind = (s.get("engine") or {}).get("kind")
    if kind != ENGINE:
        raise RuntimeError(f"the app runs the {kind} engine, not {ENGINE} ({s.get('engine')})")
    # One stage per app: the second app's windows to stage 2, and so on.
    for n, (_, _, bundle, _) in list(enumerate(apps(), start=1))[1:]:
        for w in [w for w in s["windows"] if w.get("bundle_id") == bundle and w["role"] == "standard"]:
            T.do("send", n, w["id"])
    T.do("switch", 1)
    front_first_app()
    s = T.state()
    sizes = sorted(len(v) for v in T.stage_titles(s).values())
    if len(s["stages"]) != STAGES or sizes != [2] * STAGES:
        raise RuntimeError(f"wanted {STAGES} stages of 2 windows, got {T.stage_titles(s)}")
    T.pointer_off_strip(s, "the measurements")
    return s


# ----- rest ------------------------------------------------------------------------------

def scenario_rest():
    say(f"\n== rest ({ENGINE}, run {RUN}): {STAGES} stages, {SETTLE_S:.0f} s settle, {REST_S:.0f} s untouched ==")
    stages_up()
    time.sleep(SETTLE_S)
    sp = Span(with_top_secs=REST_S)
    time.sleep(REST_S)
    m = sp.end()
    return record("rest", m)


# ----- sample ----------------------------------------------------------------------------

IDLE_LEAVES = {
    "mach_msg2_trap", "mach_msg_trap", "__psynch_cvwait", "__psynch_mutexwait", "__semwait_signal", "kevent",
    "kevent64", "kevent_id", "__workq_kernreturn", "__select", "__pselect", "__recvfrom", "__recvmsg", "read",
    "__read_nocancel", "__accept", "accept", "poll", "__ulock_wait", "__ulock_wait2", "__sigsuspend", "__wait4",
    "semaphore_wait_trap", "semaphore_timedwait_trap", "__sigwait", "start_wqthread", "thread_start",
    "__open_nocancel", "__pthread_kill", "__wait4_nocancel", "__sigsuspend_nocancel", "__waitid",
}

_V0_HEAD = re.compile(r"^_R(?:I|N[A-Za-z])*C(?:s[0-9A-Za-z]*_)?")
_V0_IDENT = re.compile(r"(?:s[0-9A-Za-z]*_)?u?(\d+)_?")


def demangle(sym):
    """A Rust v0 symbol's path, crate first (`stage_manager::platform::wm_thread::tick`), from
    its crate root and the plain identifiers after it; anything else as it is."""
    m = _V0_HEAD.match(sym)
    if not m:
        return sym
    i, parts = m.end(), []
    while i < len(sym):
        n = _V0_IDENT.match(sym, i)
        if not n:
            break
        length, start = int(n.group(1)), n.end()
        ident = sym[start:start + length]
        if len(ident) < length or not re.fullmatch(r"[A-Za-z0-9_]*", ident):
            break
        parts.append(ident or "{closure}")
        i = start + length
    return "::".join(parts) if parts else sym


_LINE = re.compile(r"^(?P<lead>[ +!:|]*)(?P<count>\d+) (?P<rest>.+)$")


def parse_sample(path):
    """The call graph of a `sample` file: for every stack with samples of its own, (thread,
    frames root first as (symbol, library), own samples). Also the sampling interval."""
    with open(path, encoding="utf-8", errors="replace") as f:
        text = f.read()
    m = re.search(r"every (\d+(?:\.\d+)?) millisecond", text)
    interval = float(m.group(1)) if m else 1.0
    body = text.split("Call graph:", 1)[-1]
    body = re.split(r"\n(?:Total number in stack|Binary Images:|Sort by top of stack)", body, maxsplit=1)[0]
    leaves, stack = [], []  # stack: [depth, count, children total, label, lib]

    def close(entry, thread):
        own = entry[1] - entry[2]
        if own > 0 and thread is not None:
            leaves.append((thread, [(e[3], e[4]) for e in stack] + [(entry[3], entry[4])], own))

    thread = None
    for line in body.splitlines():
        lm = _LINE.match(line)
        if not lm:
            continue
        depth, count, rest = len(lm.group("lead")), int(lm.group("count")), lm.group("rest")
        while stack and stack[-1][0] >= depth:
            e = stack.pop()
            close(e, thread)
        if depth <= 4:
            name = rest.split(":", 1)[1].strip() if ":" in rest else rest
            thread = "main" if "com.apple.main-thread" in rest else re.sub(r"\s+\(serial\)$", "", name.split("   ")[-1].strip())
            if thread.startswith("Thread_"):
                thread = "unnamed"
            stack = [[depth, count, 0, "<thread>", ""]]
            continue
        sym, lib = rest, ""
        if "  (in " in rest:
            sym, tail = rest.split("  (in ", 1)
            lib = tail.split(")", 1)[0]
        sym = re.sub(r"\s+\+ \d+.*$", "", sym).strip()
        if stack:
            stack[-1][2] += count
        stack.append([depth, count, 0, demangle(sym), lib])
    while stack:
        e = stack.pop()
        close(e, thread)
    return leaves, interval


# What started the work (the outermost marker found walking from the root) and what the work
# is (keywords anywhere in the stack, first match wins).
SOURCES = [
    ("timer", "__CFRUNLOOP_IS_CALLING_OUT_TO_A_TIMER_CALLBACK_FUNCTION__"),
    ("main queue", "__CFRUNLOOP_IS_SERVICING_THE_MAIN_DISPATCH_QUEUE__"),
    ("run-loop source (mach port: AX notices, window server, events)", "__CFRUNLOOP_IS_CALLING_OUT_TO_A_SOURCE1_PERFORM_FUNCTION__"),
    ("run-loop source (posted)", "__CFRUNLOOP_IS_CALLING_OUT_TO_A_SOURCE0_PERFORM_FUNCTION__"),
    ("run-loop observer (Core Animation commit, AppKit display)", "__CFRUNLOOP_IS_CALLING_OUT_TO_AN_OBSERVER_CALLBACK_FUNCTION__"),
    ("dispatch queue", "_dispatch_client_callout"),
]
KINDS = [
    ("capture", ("ScreenCaptureKit", "SCScreenshot", "SCShareable", "SCStream", "capture", "snapshot", "thumbnail", "replayd")),
    ("Accessibility", ("HIServices", "AXUIElement", "AXObserver", "::ax::", "_AX")),
    ("window list", ("CGWindowList", "SLSCopyWindow", "SLSCopySpaces", "windows::list_windows", "windows::on_screen", "windows::describe", "CGSCopyWindow")),
    ("drawing (Core Animation, AppKit, Core Graphics)", ("QuartzCore", "CoreGraphics", "AppKit", "CoreImage", "Metal", "IOSurface")),
    ("logging", ("tracing", "logging", "os_log")),
]


def ours(frames, after=0, inner=False):
    """A frame of our own code below index `after`, without the crate name: the outermost
    (what the source called) or the innermost (where the time went); closures and generic
    helpers skipped where a named function is there."""
    names = [s for s, lib in frames[after:] if s.startswith("stage_manager::")]
    named = [s for s in names if not s.endswith("{closure}")] or names
    if not named:
        return None
    pick = named[-1] if inner else named[0]
    return pick.split("::", 1)[1]


PLUMBING = ("libsystem", "libdispatch", "libxpc", "Foundation", "CoreFoundation", "dyld", "libobjc", "stage-manager")


def framework(frames, after=0):
    """With no frame of ours: the first frame of a framework that is not plumbing (dispatch,
    XPC, Foundation), as "Library: symbol" (ScreenCaptureKit's listing, ReplayKit's replies)."""
    rest = frames[after:]
    for want in ("ScreenCaptureKit", "HIServices", "QuartzCore", "SkyLight"):
        for sym, lib in rest:
            if lib == want:
                return f"{lib}: {sym[:70]}"
    for sym, lib in rest:
        if lib and not lib.startswith(PLUMBING):
            return f"{lib}: {sym[:70]}"
    return None


def classify(path):
    leaves, interval = parse_sample(path)
    active = [(th, fr, n) for th, fr, n in leaves if fr[-1][0].split()[0] not in IDLE_LEAVES]
    total = sum(n for _, _, n in active)
    by_source, by_kind, by_thread, by_fn = {}, {}, {}, {}
    for th, fr, n in active:
        names = [s for s, _ in fr]
        src, at = None, 0
        for label, marker in SOURCES:
            if marker in names:
                src, at = label, names.index(marker)
                break
        fn = ours(fr, at)
        inner = ours(fr, at, inner=True)
        key = f"{th} | {src or 'thread body'} | {fn or framework(fr, at) or '-'}"
        by_source[key] = by_source.get(key, 0) + n
        stack = " ".join(f"{s} {lib}" for s, lib in fr)
        kind = next((k for k, words in KINDS if any(w in stack for w in words)), "other")
        by_kind[kind] = by_kind.get(kind, 0) + n
        by_thread[th] = by_thread.get(th, 0) + n
        if inner:
            by_fn[inner] = by_fn.get(inner, 0) + n
    top = lambda d, k=15: [[name, round(c * interval), round(100 * c / max(total, 1), 1)] for name, c in sorted(d.items(), key=lambda x: -x[1])[:k]]  # noqa: E731
    return {
        "interval_ms": interval, "active_ms": round(total * interval),
        "by_kind": top(by_kind), "by_thread": top(by_thread), "by_source": top(by_source, 25), "by_function": top(by_fn, 20),
    }


def scenario_sample():
    if ENGINE == "off":
        return None
    say(f"\n== sample ({ENGINE}, run {RUN}): {REST_S:.0f} s of `sample` at rest ==")
    if not T.app_pid():
        stages_up()
        time.sleep(SETTLE_S)
    path = os.path.join(OUT, f"sample-{ENGINE}-{RUN}.txt")
    sp = Span()
    r = subprocess.run(["sample", str(T.app_pid()), str(int(REST_S)), "1", "-mayDie", "-file", path],
                       capture_output=True, text=True)
    m = sp.end()
    if r.returncode != 0 or not os.path.exists(path):
        return record("sample", {"ok": False, "error": (r.stderr or r.stdout)[-300:]})
    c = classify(path)
    say("  by kind:   " + "; ".join(f"{k} {ms} ms ({p} %)" for k, ms, p in c["by_kind"]))
    say("  by thread: " + "; ".join(f"{k} {ms} ms ({p} %)" for k, ms, p in c["by_thread"]))
    for k, ms, p in c["by_source"][:12]:
        say(f"    {ms:6d} ms {p:5.1f} %  {k}")
    return record("sample", {"ok": True, "file": path, "app_cpu_pct_while_sampled": m.get("app_cpu_pct"), **c})


# ----- switch ----------------------------------------------------------------------------

def specs(ws):
    return ";".join(f"{w['id']},{w['restore']['x']:.1f},{w['restore']['y']:.1f},{w['restore']['w']:.1f},{w['restore']['h']:.1f}" for w in ws)


def stage_windows(s, number):
    return [w for w in s["windows"] if w["home"] == number and w["role"] == "standard"]


def next_target(s, i, how):
    """The switch's command and target stage: a card (slots in turn) or a Dock item (the
    other stages' apps in turn)."""
    cards = s.get("strip_cards") or []
    if not cards:
        return None, None
    if how == "card":
        c = cards[i % len(cards)]
        f = c["frame"]
        if T.card_under_window(s, f):
            return None, c["number"]
        return [T.BIN, "ctl", "debug-click", f"{f['x'] + f['w'] / 2:.1f}", f"{f['y'] + f['h'] / 2:.1f}"], c["number"]
    others = [c["number"] for c in cards]
    target = others[i % len(others)]
    bundle = stage_windows(s, target)[0].get("bundle_id")
    return [T.BIN, "ctl", "debug-dock-click", bundle], target


def timed_switches(how):
    out = []
    for i in range(SWITCHES):
        s = T.state()
        cmd, target = next_target(s, i, how)
        if cmd is None:
            out.append({"ok": False, "error": f"no card to click for stage {target}"})
            continue
        incoming = stage_windows(s, target)
        outgoing = stage_windows(s, s["active"])
        r = subprocess.run([T.BIN, "probe-spaces", "settle", "--in", specs(incoming), "--out", specs(outgoing),
                            "--for", "1500", "--then", *cmd], env=T.ENV, capture_output=True, text=True, timeout=30)
        try:
            o = json.loads(r.stdout.strip().splitlines()[-1])
        except (ValueError, IndexError):
            o = {"ok": False, "error": (r.stdout + r.stderr)[-300:]}
        o["switched"] = T.wait_state(lambda x: x["active"] == target, f"stage {target} in front", timeout=4) is not None
        out.append(o)
        say(f"  {how} {i + 1}: stage {target}: last incoming at its frame {o.get('last_in_ms')} ms, outgoing gone "
            f"{o.get('last_out_ms')} ms (command {o.get('cmd_ms')} ms; mean sample {o.get('mean_interval_ms')} ms)"
            + ("" if o["switched"] else " NOT SWITCHED"))
        time.sleep(1.0)
    return out


def cpu_switches(how):
    out = []
    for i in range(SWITCHES):
        s = T.state()
        cmd, target = next_target(s, i, how)
        if cmd is None:
            continue
        sp = Span()
        subprocess.run(cmd, env=T.ENV, capture_output=True, text=True, timeout=15)
        time.sleep(max(0.0, SWITCH_CPU_S - (time.time() - sp.t0)))
        m = sp.end()
        m["switched"] = T.state(landed=False)["active"] == target
        out.append(m)
        time.sleep(1.0)
    return out


def med(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs), 1) if xs else None


def scenario_switch():
    if ENGINE == "off":
        return None
    say(f"\n== switch ({ENGINE}, run {RUN}): {SWITCHES} card clicks and {SWITCHES} Dock clicks, timed, then for CPU ==")
    if not T.app_pid():
        stages_up()
        time.sleep(2)
    data = {}
    for how in ("card", "dock"):
        timed = timed_switches(how)
        cpu = cpu_switches(how)
        ok = [o for o in timed if o.get("ok") and o.get("switched")]
        ends = [o["last_in_ms"] for o in ok if o.get("last_in_ms") is not None]
        data[how] = {
            "n": len(timed), "switched": len(ok), "settled": len(ends),
            "end_ms_median": med(ends), "end_ms_min": min(ends, default=None), "end_ms_max": max(ends, default=None),
            "end_ms": ends,
            "out_ms_median": med([o.get("last_out_ms") for o in ok]),
            "cmd_ms_median": med([o.get("cmd_ms") for o in ok]),
            "sample_ms_mean": med([o.get("mean_interval_ms") for o in ok]),
            "cpu_n": len(cpu), "cpu_switched": sum(1 for m in cpu if m["switched"]),
            "app_ms_median": med([m.get("app_cpu_ms") for m in cpu]),
            "ws_ms_median": med([m.get("ws_cpu_ms") for m in cpu]),
            "replayd_ms_median": med([m.get("replayd_cpu_ms") for m in cpu]),
            "window_s": SWITCH_CPU_S,
        }
        say(f"  {how}: end median {data[how]['end_ms_median']} ms ({data[how]['end_ms_min']}-{data[how]['end_ms_max']}), "
            f"{len(ends)} of {len(timed)} settled; CPU over {SWITCH_CPU_S} s: app {data[how]['app_ms_median']} ms, "
            f"window server {data[how]['ws_ms_median']} ms, replayd {data[how]['replayd_ms_median']} ms")
    return record("switch", data)


# ----- call ------------------------------------------------------------------------------

def clock_lines(path):
    rows = []
    try:
        with open(path) as f:
            for line in f:
                p = line.split()
                if len(p) == 5:
                    rows.append((float(p[0]), int(p[1]), int(p[2]), float(p[3]), float(p[4])))
    except FileNotFoundError:
        pass
    return rows


def clock_between(path, t0, t1):
    rows = [r for r in clock_lines(path) if t0 <= r[0] <= t1]
    if len(rows) < 2:
        return {"ok": False}
    a, b = rows[0], rows[-1]
    span = b[0] - a[0]
    frame_ms = statistics.median(r[4] for r in rows)
    frames, missed = b[1] - a[1], b[2] - a[2]
    return {
        "ok": True, "span_s": round(span, 1), "frames": frames, "missed": missed,
        "missed_per_min": round(60 * missed / span, 1) if span else None,
        "expected": round(span * 1000 / frame_ms) if frame_ms else None,
        "longest_gap_ms": round(max(r[3] for r in rows[1:]), 1), "frame_ms": round(frame_ms, 2),
    }


def scenario_call():
    say(f"\n== call ({ENGINE}, run {RUN}): a frame clock in front, {REST_S:.0f} s at rest, then {SWITCHES} activations ==")
    if ENGINE == "off" or not T.app_pid():
        stages_up()
    stop_clock()
    clock_app = os.path.join(T.ROOT, "target", T.FIX_NAME + " Clock.app")
    path = os.path.join(OUT, f"clock-{ENGINE}-{RUN}.txt")
    if os.path.exists(path):
        os.remove(path)
    subprocess.run(["open", "-n", "-g", clock_app, "--args", "--windows", "0", "--title", "Call", "--background",
                    "--frame-clock", path], check=True)
    if not T.wait_until(lambda: len(clock_lines(path)) > 4, 15, "the frame clock"):
        return record("call", {"ok": False, "error": "the frame clock did not start"})
    time.sleep(SETTLE_S)
    t0 = time.time()
    time.sleep(REST_S)
    rest = clock_between(path, t0, time.time())
    order = [a[0] for a in apps()]
    t1 = time.time()
    for i in range(SWITCHES):
        subprocess.run(["open", order[(i + 1) % len(order)]], check=True)
        time.sleep(2.5)
    switching = clock_between(path, t1, time.time())
    stop_clock()
    say(f"  rest: {rest.get('missed')} of {rest.get('expected')} refreshes missed, longest gap {rest.get('longest_gap_ms')} ms; "
        f"switching: {switching.get('missed')} of {switching.get('expected')} missed, longest gap {switching.get('longest_gap_ms')} ms")
    return record("call", {"ok": rest.get("ok") and switching.get("ok"), "rest": rest, "switching": switching})


# ----- launch ----------------------------------------------------------------------------

def scenario_launch():
    if ENGINE == "off":
        return None
    say(f"\n== launch ({ENGINE}, run {RUN}): Charlie opened {LAUNCHES} times over Alpha's stage ==")
    stop_everything()
    fresh_home()
    T.start_extra("a", 1)
    s = T.start_app(1, capture=True)
    if s is None:
        return record("launch", {"ok": False, "error": "the app did not come up"})
    results = []
    for i in range(LAUNCHES):
        T.activate("a")
        if not T.wait_state(lambda x: T.active_titles(x) == {"Alpha 1"}, "Alpha's stage in front"):
            break
        time.sleep(0.8)
        r = T.run([T.BIN, "probe-spaces", "overlap", T.EXTRA["a"][0], T.EXTRA["c"][1], T.EXTRA["c"][0], "--for", "2500",
                   "--args", "--windows", "1", "--title", "Charlie"], timeout=30)
        try:
            o = json.loads(r.stdout.strip().splitlines()[-1])
        except (ValueError, IndexError):
            o = {"ok": False}
        results.append(o)
        o["staged"] = T.wait_state(lambda x: T.active_titles(x) == {"Charlie 1"}, "Charlie's stage in front") is not None
        say(f"  launch {i + 1}: {o.get('overlaps')} overlapping samples ({o.get('overlap_ms')} ms), Charlie on screen at "
            f"{o.get('first_new_ms')} ms; Charlie's stage {'in front' if o['staged'] else 'NOT in front'}")
        T.stop_extra("c")
        T.wait_state(lambda x: "Charlie 1" not in T.by_title(x), "Charlie gone", timeout=5)
    ok = [o for o in results if o.get("ok")]
    over = [o["overlap_ms"] for o in ok if o.get("overlaps")]
    return record("launch", {
        "n": len(ok), "staged": sum(1 for o in ok if o.get("staged")), "overlapped": len(over), "overlap_ms_median": med(over), "overlap_ms": over,
        "first_new_ms_median": med([o.get("first_new_ms") for o in ok]),
    })


# ----- summary ---------------------------------------------------------------------------

def spread(xs):
    xs = [x for x in xs if isinstance(x, (int, float))]
    if not xs:
        return "-"
    m = statistics.median(xs)
    fmt = (lambda v: f"{v:.2f}") if max(abs(x) for x in xs) < 10 else (lambda v: f"{v:.0f}")
    return f"{fmt(m)} ({fmt(min(xs))}-{fmt(max(xs))})" if len(xs) > 1 else fmt(m)


def summary(out):
    rows = [json.loads(line) for line in open(os.path.join(out, "measure.jsonl")) if line.strip()]
    engines = [e for e in ("off", "corner", "spaces") if any(r["engine"] == e for r in rows)]
    pick = lambda sc, e: [r for r in rows if r["scenario"] == sc and r["engine"] == e]  # noqa: E731
    lines = [("number", *engines)]

    def add(label, sc, get):
        vals = [spread([get(r) for r in pick(sc, e)]) for e in engines]
        if any(v != "-" for v in vals):
            lines.append((label, *vals))

    g = lambda *ks: (lambda r: _dig(r, ks))  # noqa: E731
    add("rest: app CPU %", "rest", g("app_cpu_pct"))
    add("rest: app + guardian CPU %", "rest", lambda r: (r.get("app_cpu_pct") or 0) + (r.get("guardian_cpu_pct") or 0) if "app_cpu_pct" in r else None)
    add("rest: app wakeups/s (idle + interrupt)", "rest", lambda r: (r.get("app_idle_wakeups_s") or 0) + (r.get("app_intr_wakeups_s") or 0) if "app_cpu_pct" in r else None)
    add("rest: app billed energy mW", "rest", g("app_energy_mw"))
    add("rest: app POWER (top)", "rest", g("top_app_power"))
    add("rest: window server CPU %", "rest", g("ws_cpu_pct"))
    add("rest: window server POWER (top)", "rest", g("top_ws_power"))
    add("rest: replayd CPU %", "rest", g("replayd_cpu_pct"))
    add("sample: active ms in 60 s", "sample", g("active_ms"))
    for how in ("card", "dock"):
        add(f"switch {how}: click to last frame ms", "switch", g(how, "end_ms_median"))
        add(f"switch {how}: outgoing gone ms", "switch", g(how, "out_ms_median"))
        add(f"switch {how}: app CPU ms/switch", "switch", g(how, "app_ms_median"))
        add(f"switch {how}: window server CPU ms/switch", "switch", g(how, "ws_ms_median"))
        add(f"switch {how}: replayd CPU ms/switch", "switch", g(how, "replayd_ms_median"))
    add("call at rest: missed refreshes/min", "call", g("rest", "missed_per_min"))
    add("call at rest: longest gap ms", "call", g("rest", "longest_gap_ms"))
    add("call switching: missed refreshes/min", "call", g("switching", "missed_per_min"))
    add("call switching: longest gap ms", "call", g("switching", "longest_gap_ms"))
    add("launch: launches overlapped", "launch", g("overlapped"))
    add("launch: overlap ms", "launch", g("overlap_ms_median"))
    add("launch: new window on screen ms", "launch", g("first_new_ms_median"))
    widths = [max(len(str(l[i])) for l in lines) for i in range(len(lines[0]))]
    for i, l in enumerate(lines):
        print("| " + " | ".join(str(c).ljust(w) for c, w in zip(l, widths)) + " |")
        if i == 0:
            print("|" + "|".join("-" * (w + 2) for w in widths) + "|")
    runs = {e: sorted({r["run"] for r in rows if r["engine"] == e}) for e in engines}
    print(f"\nmedian (min-max) over runs {runs}")
    for e in engines:
        for r in pick("sample", e):
            if r.get("ok"):
                print(f"\nsample {e} run {r['run']}: " + "; ".join(f"{k} {p} %" for k, _, p in r["by_kind"]))


def _dig(r, ks):
    for k in ks:
        if not isinstance(r, dict):
            return None
        r = r.get(k)
    return r


# ----- main ------------------------------------------------------------------------------

def main():
    if ARGS["cmd"] == "summary":
        summary(ARGS["out"])
        return 0
    os.makedirs(OUT, exist_ok=True)
    if T.owner_app_running():
        say("STOP: another Stage Manager (the owner's) is running; nothing is measured beside it")
        return 1
    say(f"home: {T.HOME}; engine {ENGINE}; run {RUN}; out {OUT}")
    front = T.frontmost_bundle()
    fns = {"rest": scenario_rest, "sample": scenario_sample, "switch": scenario_switch, "call": scenario_call, "launch": scenario_launch}
    code = 0
    try:
        for sc in ARGS["scenarios"]:
            try:
                fns[sc]()
            except Exception as e:  # noqa: BLE001 - one scenario failing leaves the others
                code = 1
                record(sc, {"ok": False, "error": repr(e)})
    finally:
        T.stop_app()
        if T.pids_of(T.FIX_EXE) or any(T.pids_of(e[2]) for e in T.EXTRA.values()):
            T.all_onscreen("cleanup")
        stop_everything()
        if front and front not in T.ALL_IDS:
            subprocess.run(["open", "-b", front], capture_output=True)
    return code


if __name__ == "__main__":
    sys.exit(main())
