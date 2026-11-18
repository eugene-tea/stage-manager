#!/usr/bin/env python3
"""Offline unit tests for the engines' measurement (scripts/measure_engines.py and the parts of
scripts/measure_stages.py it adds): the plan and its dry run, the p90, the hidden window's
round read from its frame clock, and the corner-against-spaces table. Nothing is launched.
Run: python3 scripts/test_measure_unit.py
"""

import io
import os
import subprocess
import sys
import unittest
from contextlib import redirect_stdout

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import measure_engines as E  # noqa: E402
import measure_stages as M  # noqa: E402

FRAME = 8.33  # ms, 120 Hz


def clock(t0, t1, fps=120.0, frames0=0, step=0.25, gap_at=None):
    """Frame clock lines (time, frames, missed, longest gap ms, frame ms) from t0 to t1, one
    every `step` s while drawing at `fps`; `gap_at` = (start, end): no line and no frame
    between them, the first line after carrying the gap as its longest."""
    rows, t, frames = [], t0, frames0
    while t <= t1:
        if gap_at and gap_at[0] < t < gap_at[1]:
            t = gap_at[1]
            rows.append((t, frames + 1, 0, (gap_at[1] - gap_at[0]) * 1000, FRAME))
            frames += 1
            t += step
            continue
        rows.append((t, frames, 0, FRAME, FRAME))
        frames += int(fps * step)
        t += step
    return rows


class P90(unittest.TestCase):
    def test_nearest_rank(self):
        self.assertEqual(M.p90(list(range(1, 11))), 9)
        self.assertEqual(M.p90(list(range(1, 21))), 18)
        self.assertEqual(M.p90([5]), 5)

    def test_none_and_empty(self):
        self.assertIsNone(M.p90([]))
        self.assertIsNone(M.p90([None, None]))
        self.assertEqual(M.p90([None, 3.0, 1.0]), 3.0)


class HiddenRound(unittest.TestCase):
    def test_a_clock_that_kept_drawing(self):
        rows = clock(100.0, 110.0)
        o = M.hidden_round(rows, 101.0, 106.0, 1.5)
        self.assertTrue(o["ok"])
        self.assertFalse(o["paused"])
        self.assertAlmostEqual(o["hidden_fps"], 120.0, delta=10)
        self.assertLessEqual(o["back_ms"], 250.0)
        self.assertEqual(o["after"]["missed"], 0)

    def test_a_clock_paused_while_hidden(self):
        # Drawing until the hide, nothing until 0.3 s after the show, smooth from then on.
        rows = clock(100.0, 101.0) + clock(106.3, 110.0, frames0=1000)
        rows[len(clock(100.0, 101.0))] = (106.3, 1000, 600, 5300.0, FRAME)
        o = M.hidden_round(rows, 101.0, 106.0, 1.5)
        self.assertTrue(o["ok"])
        self.assertTrue(o["paused"])
        self.assertEqual(o["hidden_fps"], 0.0)
        self.assertAlmostEqual(o["back_ms"], 300.0, delta=1)
        self.assertEqual(o["first_gap_ms"], 5300.0)
        self.assertAlmostEqual(o["smooth_ms"], 550.0, delta=1)

    def test_no_line_after_the_show(self):
        o = M.hidden_round(clock(100.0, 104.0), 101.0, 106.0, 1.5)
        self.assertFalse(o["ok"])

    def test_clock_span_counts_over_the_span_only(self):
        rows = [(1.0, 0, 0, 8.3, 8.33), (1.25, 30, 0, 8.3, 8.33), (1.5, 58, 2, 25.0, 8.33), (9.0, 900, 9, 99.0, 8.33)]
        c = M.clock_span(rows, 1.0, 2.0)
        self.assertEqual((c["frames"], c["missed"], c["longest_gap_ms"]), (58, 2, 25.0))
        self.assertFalse(M.clock_span(rows[:1], 0, 2)["ok"])


def rows_for(engine, run, rest_cpu, ends, overlaps):
    return [
        {"scenario": "rest", "engine": engine, "run": run, "app_cpu_pct": rest_cpu, "guardian_cpu_pct": 0.01,
         "ws_cpu_pct": 1.5, "top_app_power": 0.4, "top_ws_power": 3.0, "app_energy_mw": 2.0},
        {"scenario": "switch", "engine": engine, "run": run,
         "ctl": {"end_ms": ends, "out_ms_median": 50.0, "app_ms_median": 20.0, "ws_ms_median": 30.0}},
        {"scenario": "launch", "engine": engine, "run": run, "n": 10, "overlapped": len(overlaps), "overlap_ms": overlaps},
        {"scenario": "hidden", "engine": engine, "run": run, "ok": True, "hidden_fps_median": 0.0, "back_ms_median": 40.0,
         "smooth_ms_median": 290.0, "after_missed_median": 3},
    ]


class Table(unittest.TestCase):
    def test_corner_against_spaces(self):
        rows = rows_for("corner", 1, 0.5, [300, 320, 340], [90, 110]) + rows_for("spaces", 1, 0.2, [40, 50, 400], [])
        t = E.table(rows)
        self.assertEqual(list(t), [label for label, _, _ in E.ROWS])
        self.assertEqual(t["App CPU % at rest (3 stages, 60 s)"], {"corner": 0.5, "spaces": 0.2})
        self.assertEqual(t["App and guardian CPU % at rest"]["corner"], 0.51)
        self.assertEqual(t["Switch, command to last frame, median ms"], {"corner": 320, "spaces": 50})
        self.assertEqual(t["Switch, command to last frame, p90 ms"], {"corner": 340, "spaces": 400})
        self.assertEqual(t["New app: launches with an overlap"], {"corner": "2 of 10", "spaces": "0 of 10"})
        self.assertEqual(t["New app: overlap, median ms"], {"corner": 100.0, "spaces": None})
        self.assertIsNone(t["Call in front, switching: refreshes missed/min"]["corner"], "not run: no value")

    def test_runs_pool_their_samples_and_median_the_rest(self):
        rows = rows_for("corner", 1, 0.4, [100, 200], [10]) + rows_for("corner", 2, 0.6, [300, 400], [30])
        t = E.table(rows)
        self.assertEqual(t["App CPU % at rest (3 stages, 60 s)"]["corner"], 0.5)
        self.assertEqual(t["Switch, command to last frame, median ms"]["corner"], 250)
        self.assertEqual(t["Switch, command to last frame, p90 ms"]["corner"], 400)

    def test_a_failed_scenario_gives_no_value_and_is_listed(self):
        rows = rows_for("corner", 1, 0.5, [1], [])
        rows.append({"scenario": "rest", "engine": "spaces", "run": 1, "ok": False,
                     "error": "RuntimeError('the app runs the corner engine, not spaces')"})
        self.assertIsNone(E.table(rows)["App CPU % at rest (3 stages, 60 s)"]["spaces"])
        self.assertEqual([f[:3] for f in E.failures(rows)], [("spaces", 1, "rest")])

    def test_markdown_has_a_row_per_number(self):
        md = E.markdown(E.table(rows_for("corner", 1, 0.5, [1], [])))
        lines = md.splitlines()
        self.assertEqual(len(lines), 2 + len(E.ROWS))
        self.assertIn("corner", lines[0])
        self.assertIn("spaces", lines[0])


class Plan(unittest.TestCase):
    def test_captures_off_three_stages_ctl_switches_both_engines(self):
        o = E.parse_args(["--runs", "2", "--switches", "5"])
        p = E.plan(o)
        env = p["env"]
        self.assertEqual(env["ENGINES"], "corner spaces")
        self.assertEqual((env["SM_CAPTURE"], env["SM_STAGES"], env["SM_SWITCH_WAYS"]), ("0", "3", "ctl"))
        self.assertEqual((env["RUNS"], env["SM_SWITCHES"]), ("2", "5"))
        self.assertTrue(p["cmd"][0].endswith("scripts/measure.sh"))
        self.assertEqual(p["cmd"][1:], E.SCENARIOS)
        self.assertIn(os.path.join("target", "measure-engines"), o["out"])
        self.assertFalse(any("STAGE_MANAGER_SPACES" in k for k in env))

    def test_scenarios_named_and_unknown_refused(self):
        self.assertEqual(E.plan(E.parse_args(["rest", "hidden"]))["cmd"][1:], ["rest", "hidden"])
        with self.assertRaises(SystemExit):
            E.parse_args(["nonsense"])
        with self.assertRaises(SystemExit):
            E.parse_args(["--runs", "0"])

    def test_the_dry_run_launches_nothing(self):
        def refuse(*a, **k):
            raise AssertionError(f"launched {a}")

        saved = subprocess.run, subprocess.Popen
        subprocess.run = subprocess.Popen = refuse
        try:
            buf = io.StringIO()
            with redirect_stdout(buf):
                self.assertEqual(E.main(["--dry-run", "--out", "/nonexistent/sm-measure"]), 0)
        finally:
            subprocess.run, subprocess.Popen = saved
        text = buf.getvalue()
        self.assertIn("SM_CAPTURE=0", text)
        self.assertIn("run 1, corner", text)
        self.assertIn("run 1, spaces", text)
        self.assertIn("powermetrics", text)
        self.assertFalse(os.path.exists("/nonexistent/sm-measure"))

    def test_measure_stages_accepts_the_hidden_scenario_by_name_only(self):
        self.assertNotIn("hidden", M.SCENARIOS)
        self.assertIn("hidden", M.BY_NAME)
        o = M.parse_args(["run", "--engine", "spaces", "--out", "/x", "hidden"])
        self.assertEqual(o["scenarios"], ["hidden"])


if __name__ == "__main__":
    unittest.main()
