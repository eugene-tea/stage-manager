# Stage Manager

Everything Apple should have done to Stage Manager three or four years ago, done now, and
done the Apple way. Out of the box it looks and behaves like Apple's own. Every addition stays
out of sight until you reach for it, and every knob lives in a config file.

A macOS Stage Manager written in Rust. Its author uses it every day, and it is still in
development. It is not affiliated with Apple.

`make install` puts it in `~/Applications`. The full guide is [docs/GUIDE.md](docs/GUIDE.md),
and the plan is [PLAN.md](PLAN.md).

## Progress

- **2026-10-12** — Day one of daily use. The strip matches Apple's down to the card tilt,
  and each Desktop gets its own stages and strip. Switches animate. Cards can be reordered,
  and dropping one card on another merges the two stages. The strip hides during Mission
  Control. Safety layers mean a window is never left hidden, not even after a crash. Logs
  are kept per day, with a "Mark a Problem" menu item. Two full code reviews are done. The
  trial week starts.
