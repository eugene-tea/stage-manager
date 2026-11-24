#!/bin/bash
# Scripted checks (see scripts/test_stages.py): builds the app and private fixtures ("SM
# Fixture Stages.app", bundle ID dev.eugene.stagemanager.fixture.stages, plus "SM Fixture
# Stages A/B/C.app" with IDs <id>.a/.b/.c for the native phase; SM_FIXTURE_ID picks another
# base ID so no other fixture run is disturbed), then runs the phases.
#
#   scripts/test-stages.sh                 # every phase: m3 safety m4 m6 restore strip native visible
#                                          # parity motion slots expose mission capture raise park offscreen
#                                          # enhanced stale handles displays screens engine
#   scripts/test-stages.sh engine          # which window engine runs: each launch's probe and choice,
#                                          # the fallback mid-run to the Corner engine (round sm-probe)
#   scripts/test-stages.sh restore m6      # restore after a restart (simulated new sessions), and
#                                          # the same-session relaunch
#   scripts/test-stages.sh m3 safety       # some phases
#   scripts/test-stages.sh launch          # a newly opened app's window never over the stage left:
#                                          # 20 launches sampled every ms (SM_LAUNCHES=n)
#   scripts/test-stages.sh landing         # under CPU load, no incoming window in place while its
#                                          # stage's pictures fly (round sm-landing; SM_LANDING_SWITCHES=n,
#   scripts/test-stages.sh landing-abort   # a window flying its last stretch cut short, quit, killed (SM_ENGINE=spaces, STAGE_MANAGER_LANDING=live*)
#                                          # STAGE_MANAGER_LANDING=pictures|ordered|live|live-early|live-late)
#   scripts/test-stages.sh landing-order   # two overlapping windows of two apps keep their order as they
#                                          # land: the overlap's pixels and the window list every 10 ms
#                                          # (round landing-order; SM_LANDING_ORDER_SWITCHES=n; with
#                                          # STAGE_MANAGER_LANDING_LEVEL=ranked the landing Spaces get levels)
#   scripts/test-stages.sh landing-merge   # a shift-clicked card's window lands in front of the stage's
#                                          # window it overlaps: pixels and the window list every 10 ms
#                                          # (round landing-merge; SM_LANDING_MERGE_ROUNDS=n)
#   scripts/test-stages.sh behave          # an app's front window on arrival; a card dropped
#                                          # on the other screen (round parity-behave)
#   scripts/test-stages.sh screens         # two screens (skipped with one): ⌘-Tab to a card on the other
#                                          # screen, full screen there, Mission Control (PLAN live checks 3-5)
#   scripts/test-stages.sh glass           # the cards' glass (captures on): white, grey and black
#                                          # cards and a switch's snapshots against the model
#   scripts/test-stages.sh cards           # [strip] card_layout: stacked, apple and spread_covered
#                                          # cards of the three fixture apps against Apple's 11-16 layouts
#   SM_ENGINE=spaces scripts/test-stages.sh cmdtab-probe    # ⌘Tab posted, Escape before ⌘'s release (nothing activated):
#                                          # does the Dock show the switcher's selection, what the input
#                                          # monitor and an event tap see (round cmd-tab; not a default phase)
#   SM_ENGINE=spaces scripts/test-stages.sh mission         # Mission Control and App Exposé show the hidden stages'
#                                          # windows and put them back; a pick; kill -9 with them out
#                                          # (round spaces-mission-control; skips under the park corner)
#   SM_ENGINE=spaces scripts/test-stages.sh mission-desktop # a Desktop picked inside Mission Control (⌃→ there),
#                                          # Mission Control opened again at once: the shown Desktop's
#                                          # hidden windows still put home (needs a Desktop to the right;
#                                          # not a default phase)
#   SM_ENGINE=spaces scripts/test-stages.sh mission-probe   # does Mission Control lay out a hidden window put on
#                                          # the Desktop before its key, after it shows, at the key? Prints
#                                          # what each outcome means (not a default phase; SM_SHOTS=<dir> adds
#                                          # by-eye screenshots)
#   SM_ENGINE=spaces scripts/test-stages.sh display-gone [main|secondary] [--rounds N]
#                                          # a screen going away keeps every stage's windows: a virtual
#                                          # screen (scripts/virtual-display) added, fixture stages on it
#                                          # and on the built-in one, then removed (main: it held the menu
#                                          # bar); 65 s of looks, then every Desktop (default both, 3 rounds)
#   SM_ENGINE=spaces scripts/test-stages.sh display-gone frames [main|secondary] [--rounds N]
#                                          # [displays] frames_per_setup: windows resized on the built-in
#                                          # screen, the virtual screen added, resized there, removed: the
#                                          # built-in screen's frames back; added again: the two-screen ones
#   scripts/test-stages.sh probe-system [dock|flip|names|indicator] [--rounds N] [--window ID]
#                                          # four probes (round r0-probes): the Dock's AXExpose notices,
#                                          # when Current Space flips in a slide, the window server's
#                                          # screen and Space names, which capture lights the purple
#                                          # indicator (indicator by name only, the owner at the Mac)
#   scripts/test-stages.sh spaces          # guided two-Desktop check (asks you to switch Desktops)
#   scripts/test-stages.sh spaces --auto   # the same hands-free (SM_AUTO=1 too): the app switches
#                                          # Desktops and toggles full screen; nobody at the Mac
#   scripts/test-stages.sh spaces --auto --dry-run   # preflight and planned steps only
#   SKIP_BUILD=1 TEST_HOME=/tmp/sm scripts/test-stages.sh
#
# Every app run uses STAGE_MANAGER_HOME=$TEST_HOME (default: a new /tmp/sm-test.* dir) and
# STAGE_MANAGER_ONLY=<the fixtures>; only fixture windows ever move. Window captures are off
# (STAGE_MANAGER_NO_CAPTURE=1) in every run but the capture phase's one launch. The fixture app comes to
# the front during switches; the app that was frontmost is activated again at the end.
# No phase runs while the owner's installed app (~/Applications/Stage Manager.app) is up;
# No screen is captured unless asked (`screencapture` raises macOS's screen-capture alert):
# SM_SCREENSHOTS=<dir> turns on the visible phase's strip screenshots; SM_SHOTS=<dir> turns on
# `spaces --auto`'s by-eye screenshots and the Mission Control pixel checks (never the repo).
# SM_DOCK_REPEATS=<n> sets how many Dock arrivals `spaces --auto` makes (default 3).
# SM_ENGINE=spaces runs the app with the Spaces window engine (STAGE_MANAGER_ENGINE, a test
# switch; default the Corner engine): every behaviour check runs on both, the park corner's
# own checks skip with a reason line.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

FIX_ID="${SM_FIXTURE_ID:-dev.eugene.stagemanager.fixture.stages}"
SUFFIX="${FIX_ID##*.}"
FIX_NAME="SM Fixture $(tr '[:lower:]' '[:upper:]' <<<"${SUFFIX:0:1}")${SUFFIX:1}"
if [ -z "${SKIP_BUILD:-}" ]; then
  make sign FEATURES=probes >/dev/null || { echo "build failed" >&2; exit 1; }
fi
# The main fixture, plus three apps for the native phase (<id>.a, .b, .c). Built every run,
# and with SKIP_BUILD whenever one is missing or older than its source: an old fixture
# ignores the signals newer drills send it (SIGWINCH and SIGINFO are ignored by default).
for k in "" a b c; do
  if [ -z "$k" ]; then id="$FIX_ID"; app="target/$FIX_NAME.app"
  else id="$FIX_ID.$k"; app="target/$FIX_NAME $(tr '[:lower:]' '[:upper:]' <<<"$k").app"; fi
  exe="$app/Contents/MacOS/sm-fixture"
  if [ -n "${SKIP_BUILD:-}" ] && [ -x "$exe" ] && [ ! scripts/fixture/main.swift -nt "$exe" ]; then
    continue
  fi
  [ -n "${SKIP_BUILD:-}" ] && echo "SKIP_BUILD: $app is missing or older than scripts/fixture; rebuilding it" >&2
  FIXTURE_ID="$id" FIXTURE_APP="$app" scripts/bundle.sh fixture >/dev/null || exit 1
  scripts/sign.sh "$app" >/dev/null || exit 1
done
# `spaces` is the two-Desktop check (scripts/test_spaces.py), run alone: guided (a person at
# the Mac switches Desktops when asked) or `--auto` (hands-free). A dry run acts on nothing,
# not even the dev copy.
if [ "${1:-}" = "spaces" ]; then
  case " ${*:2} " in
    *" --dry-run "*) ;;
    *) scripts/dev-app.sh >/dev/null || exit 1 ;;
  esac
  exec python3 scripts/test_spaces.py "${@:2}"
fi
# `probe-system` (scripts/probe_system.py): the four live probes of round r0-probes, run alone.
if [ "${1:-}" = "probe-system" ]; then
  scripts/dev-app.sh >/dev/null || exit 1
  exec python3 scripts/probe_system.py "${@:2}"
fi
# `display-gone` (scripts/test_display_gone.py) adds and removes a virtual screen, run alone.
if [ "${1:-}" = "display-gone" ]; then
  scripts/dev-app.sh >/dev/null || exit 1
  exec python3 scripts/test_display_gone.py "${@:2}"
fi
# The app runs from the fixed dev copy (one screen-access approval for every worktree);
# dev-app.sh refuses to replace a copy running from there, whichever home it uses.
scripts/dev-app.sh >/dev/null || exit 1
if [ -n "${SM_ENGINE:-}" ]; then export STAGE_MANAGER_ENGINE="$SM_ENGINE"; fi
exec python3 scripts/test_stages.py "$@"
