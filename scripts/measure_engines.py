#!/usr/bin/env python3
"""The two window engines measured side by side (PLAN 1.9: CPU at rest, switch time, a new
app's overlap, Energy Impact, a drawing window's dropped frames), for docs/UX-SPEC.md
"Window engines measured".

    scripts/measure_engines.py [--dry-run] [--runs N] [--switches N] [--rest S] [--out DIR]
                               [--skip-build] [SCENARIO...]
    scripts/measure_engines.py --table DIR

Runs scripts/measure.sh (which builds and signs the app and the fixtures, puts the app in the
dev copy and runs scripts/measure_stages.py once per engine and run) with the park corner
and the Spaces engine only, window captures off (STAGE_MANAGER_NO_CAPTURE: no screen-capture
alert), three stages of two fixture windows, switches by `ctl switch` (no input posted), in
a scratch home per run (STAGE_MANAGER_HOME, STAGE_MANAGER_ONLY the fixtures): the owner's
config and windows are never touched, and nothing is measured while another Stage Manager
runs. The engine is the live suite's test switch (STAGE_MANAGER_ENGINE); a Spaces run whose
launch probe fails measures nothing: each of its scenarios is recorded as failed with the
probe's reason, and listed under the table.

Then one table, corner against spaces (median over runs; a pooled median and p90 where each
run keeps its samples), printed and written with every number to
target/measure-engines.json; the raw lines stay in DIR (target/measure-engines/<time>/ by
default). `--table DIR` prints the table again from DIR/measure.jsonl.

Scenarios (all by default): rest switch launch hidden call.
- rest: 60 s (--rest) untouched with 3 stages: the app's CPU (`proc_pid_rusage`) and its
  guardian's, the window server's (`ps` time over wall time), `top`'s POWER for the app and
  the window server (the Energy Impact column of Activity Monitor), billed energy.
- switch: N (--switches, 20) `ctl switch` commands, timed from the command to the last
  incoming window at its frame for good and the outgoing ones gone, the window list read
  every ms (`probe-spaces settle`); median and p90; then CPU per switch.
- launch: a fixture app opened 10 times over another's stage, the on-screen list read every
  ms (`probe-spaces overlap`): how long its window shows over the stage being left.
- hidden: a fixture window drawing on every display refresh (`--frame-clock-normal`) in a
  stage of its own, hidden 5 s by a switch and shown again: refreshes drawn while hidden,
  how soon after the show it draws again and smoothly, refreshes missed after.
- call: a floating call stand-in drawing on every refresh in front while other stages'
  apps are activated: refreshes missed a minute and the longest gap.

Not measured: `powermetrics` (it needs root; `top`'s POWER stands in), and frames the window
server composites or drops on screen (private tools, or screen captures, which stay off):
the drawing windows count their own display-link callbacks only.
"""

import json
import os
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import measure_stages as M  # noqa: E402  (lib mode: the pure helpers, nothing run)

ENGINES = ("corner", "spaces")
SCENARIOS = ["rest", "switch", "launch", "hidden", "call"]
SUMMARY = os.path.join(ROOT, "target", "measure-engines.json")


def parse_args(argv):
    o = {"dry_run": False, "runs": 1, "switches": 20, "rest": 60, "out": None, "skip_build": False,
         "scenarios": [], "table": None}
    it = iter(argv)
    for a in it:
        if a == "--dry-run":
            o["dry_run"] = True
        elif a == "--skip-build":
            o["skip_build"] = True
        elif a in ("--runs", "--switches", "--rest"):
            o[a[2:]] = int(next(it, "0"))
        elif a in ("--out", "--table"):
            o[a[2:]] = next(it, None)
        elif a in SCENARIOS:
            o["scenarios"].append(a)
        else:
            raise SystemExit(f"unknown argument {a}\n{__doc__}")
    if o["runs"] < 1 or o["switches"] < 1 or o["rest"] < 1:
        raise SystemExit("--runs, --switches and --rest take a positive number")
    o["scenarios"] = o["scenarios"] or list(SCENARIOS)
    o["out"] = o["out"] or os.path.join(ROOT, "target", "measure-engines", time.strftime("%Y%m%d-%H%M%S"))
    return o


def plan(o):
    """The environment and the command line of the run (scripts/measure.sh), and what each
    number is: what --dry-run prints and the run runs."""
    env = {
        "ENGINES": " ".join(ENGINES),
        "RUNS": str(o["runs"]),
        "OUT": o["out"],
        "SM_CAPTURE": "0",
        "SM_STAGES": "3",
        "SM_SWITCH_WAYS": "ctl",
        "SM_SWITCHES": str(o["switches"]),
        "SM_REST_S": str(o["rest"]),
    }
    if o["skip_build"]:
        env["SKIP_BUILD"] = "1"
    return {"env": env, "cmd": [os.path.join(HERE, "measure.sh"), *o["scenarios"]]}


def say_plan(o, p):
    print("measure_engines: the plan (nothing is launched with --dry-run)")
    print("  " + " ".join(f"{k}={json.dumps(v) if ' ' in v else v}" for k, v in p["env"].items()) + " " + " ".join(p["cmd"]))
    if not o["skip_build"]:
        print("  builds and signs the app and the fixtures (make sign, scripts/bundle.sh fixture), puts the app in the dev copy")
    for run in range(1, o["runs"] + 1):
        for e in ENGINES:
            how = "STAGE_MANAGER_ENGINE=spaces (the launch probe must pass)" if e == "spaces" else "the default engine (the park corner)"
            print(f"  run {run}, {e}: a scratch home, {how}, STAGE_MANAGER_NO_CAPTURE=1; {' '.join(o['scenarios'])}")
    print(f"  then the table, corner against spaces, and every number in {os.path.relpath(SUMMARY, ROOT)}; raw lines in {o['out']}")
    print("  not measured: powermetrics (needs root; top's POWER stands in), frames the window server drops on screen")


# ----- the table -------------------------------------------------------------------------

def _dig(r, ks):
    for k in ks:
        if not isinstance(r, dict):
            return None
        r = r.get(k)
    return r


def _num(v):
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def _med(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs), 2) if xs else None


def _sum(*vals):
    vals = [_num(v) for v in vals]
    return None if vals[0] is None else round(sum(v or 0 for v in vals), 3)


# (label, scenario, how to read a run's row: the per-run value; or "pooled" with the key of
# a list of samples kept per run and "median" or "p90" of them all).
ROWS = [
    ("App CPU % at rest (3 stages, 60 s)", "rest", lambda r: _num(r.get("app_cpu_pct"))),
    ("App and guardian CPU % at rest", "rest", lambda r: _sum(r.get("app_cpu_pct"), r.get("guardian_cpu_pct"))),
    ("App wakeups/s at rest", "rest", lambda r: _sum(r.get("app_idle_wakeups_s"), r.get("app_intr_wakeups_s"))),
    ("WindowServer CPU % over the same span", "rest", lambda r: _num(r.get("ws_cpu_pct"))),
    ("Energy Impact at rest, app (top POWER)", "rest", lambda r: _num(r.get("top_app_power"))),
    ("Energy Impact at rest, WindowServer (top POWER)", "rest", lambda r: _num(r.get("top_ws_power"))),
    ("App billed energy at rest, mW", "rest", lambda r: _num(r.get("app_energy_mw"))),
    ("Switch, command to last frame, median ms", "switch", ("pooled", ("ctl", "end_ms"), "median")),
    ("Switch, command to last frame, p90 ms", "switch", ("pooled", ("ctl", "end_ms"), "p90")),
    ("Switch, outgoing gone, median ms", "switch", lambda r: _num(_dig(r, ("ctl", "out_ms_median")))),
    ("Switch, app CPU ms each", "switch", lambda r: _num(_dig(r, ("ctl", "app_ms_median")))),
    ("Switch, WindowServer CPU ms each", "switch", lambda r: _num(_dig(r, ("ctl", "ws_ms_median")))),
    ("New app: launches with an overlap", "launch", lambda r: f"{r['overlapped']} of {r['n']}" if "n" in r else None),
    ("New app: overlap, median ms", "launch", ("pooled", ("overlap_ms",), "median")),
    ("New app: overlap, p90 ms", "launch", ("pooled", ("overlap_ms",), "p90")),
    ("Hidden window: refreshes/s drawn while hidden", "hidden", lambda r: _num(r.get("hidden_fps_median"))),
    ("Hidden window: drawing again after the show, ms", "hidden", lambda r: _num(r.get("back_ms_median"))),
    ("Hidden window: drawing smoothly again, ms", "hidden", lambda r: _num(r.get("smooth_ms_median"))),
    ("Hidden window: refreshes missed in 1.5 s after", "hidden", lambda r: _num(r.get("after_missed_median"))),
    ("Call in front, switching: refreshes missed/min", "call", lambda r: _num(_dig(r, ("switching", "missed_per_min")))),
    ("Call in front, switching: longest gap ms", "call", lambda r: _num(_dig(r, ("switching", "longest_gap_ms")))),
]


def cell(rows, scenario, engine, how):
    """One engine's value of one row: the median over its runs (a text value: the runs'
    values joined), or the pooled median or p90 of every run's samples."""
    mine = [r for r in rows if r.get("scenario") == scenario and r.get("engine") == engine and r.get("ok", True) is not False]
    if isinstance(how, tuple):
        _, keys, stat = how
        pooled = [x for r in mine for x in (_dig(r, keys) or []) if _num(x) is not None]
        if not pooled:
            return None
        return _med(pooled) if stat == "median" else M.p90(pooled)
    vals = [how(r) for r in mine]
    texts = [v for v in vals if isinstance(v, str)]
    if texts:
        return ", ".join(texts)
    return _med([v for v in vals if v is not None])


def table(rows):
    """{label: {engine: value}} for every row of ROWS, in order."""
    return {label: {e: cell(rows, sc, e, how) for e in ENGINES} for label, sc, how in ROWS}


def failures(rows):
    """Each scenario run that failed: (engine, run, scenario, error)."""
    return [(r.get("engine"), r.get("run"), r.get("scenario"), r.get("error")) for r in rows if r.get("ok") is False]


def markdown(t):
    lines = [("", *ENGINES)] + [(label, *("-" if v[e] is None else str(v[e]) for e in ENGINES)) for label, v in t.items()]
    widths = [max(len(l[i]) for l in lines) for i in range(len(lines[0]))]
    out = []
    for i, l in enumerate(lines):
        out.append("| " + " | ".join(c.ljust(w) for c, w in zip(l, widths)) + " |")
        if i == 0:
            out.append("|" + "|".join("-" * (w + 2) for w in widths) + "|")
    return "\n".join(out)


def read_rows(out):
    path = os.path.join(out, "measure.jsonl")
    if not os.path.exists(path):
        return []
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def report(out):
    rows = read_rows(out)
    t = table(rows)
    print(markdown(t))
    for e, run, sc, err in failures(rows):
        print(f"  {e} run {run}: {sc} failed: {err}")
    runs = sorted({r.get("run") for r in rows})
    data = {"at": time.strftime("%Y-%m-%d %H:%M:%S"), "out": out, "runs": runs, "table": t,
            "failed": [dict(zip(("engine", "run", "scenario", "error"), f)) for f in failures(rows)], "rows": rows}
    os.makedirs(os.path.dirname(SUMMARY), exist_ok=True)
    with open(SUMMARY, "w") as f:
        json.dump(data, f, indent=1)
    print(f"\nevery number: {SUMMARY}")
    return data


def main(argv):
    o = parse_args(argv)
    if o["table"]:
        report(o["table"])
        return 0
    p = plan(o)
    say_plan(o, p)
    if o["dry_run"]:
        return 0
    os.makedirs(o["out"], exist_ok=True)
    code = subprocess.run(p["cmd"], env=dict(os.environ, **p["env"]), cwd=ROOT).returncode
    print()
    report(o["out"])
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
