#!/bin/bash
# Live check of the stand-down: Apple's Stage Manager turned on while Stage Manager runs
# brings every parked window back and stops managing; turned off again, Stage Manager takes
# back the stages it set aside, each window in its stage again. OFF BY DEFAULT: it toggles
# Apple's Stage Manager on this Mac (Apple's own then arranges every window on screen for a
# few seconds), so it runs only with
# SM_TEST_TOGGLE_APPLE_SM=1, by the owner's choice. The original GloballyEnabled value is
# restored on every exit (success, failure, Ctrl-C, kill).
#
# Needs: make dev (the dev copy) and make fixture. Manages only the fixture app
# (STAGE_MANAGER_ONLY), in a scratch STAGE_MANAGER_HOME.
set -uo pipefail
cd "$(dirname "$0")/.."

if [ "${SM_TEST_TOGGLE_APPLE_SM:-}" != "1" ]; then
  echo "skipped: set SM_TEST_TOGGLE_APPLE_SM=1 to toggle Apple's Stage Manager on this Mac (the owner's call)"
  exit 0
fi

DOMAIN=com.apple.WindowManager
KEY=GloballyEnabled
APP="${SM_DEV_APP:-$HOME/Library/Application Support/Stage Manager Dev/Stage Manager.app}"
BIN="$APP/Contents/MacOS/stage-manager"
FIXTURE="target/SM Fixture.app"
FIX_ID="dev.eugene.stagemanager.fixture"
[ -x "$BIN" ] || { echo "error: $BIN missing; run make dev" >&2; exit 1; }
[ -d "$FIXTURE" ] || { echo "error: $FIXTURE missing; run make fixture" >&2; exit 1; }
# The installed app (the default socket, before STAGE_MANAGER_HOME moves it) would stand
# down too when Apple's Stage Manager comes on: never run beside it.
if env -u STAGE_MANAGER_HOME "$BIN" ctl ping >/dev/null 2>&1; then
  echo "error: a Stage Manager is running (the installed app?); quit it first" >&2
  exit 1
fi

HOME_DIR="$(mktemp -d /tmp/sm-apple.XXXXXX)"
export STAGE_MANAGER_HOME="$HOME_DIR"

# The owner's value, exactly: absent, or a boolean.
if ORIGINAL="$(defaults read "$DOMAIN" "$KEY" 2>/dev/null)"; then HAD=1; else HAD=0; ORIGINAL=""; fi

restore() {
  local code=$?
  "$BIN" ctl quit >/dev/null 2>&1 || true
  pkill -f "SM Fixture.app/Contents/MacOS/sm-fixture" 2>/dev/null || true
  if [ "$HAD" = 1 ]; then
    if [ "$ORIGINAL" = "1" ]; then defaults write "$DOMAIN" "$KEY" -bool true; else defaults write "$DOMAIN" "$KEY" -bool false; fi
  else
    defaults delete "$DOMAIN" "$KEY" 2>/dev/null || true
  fi
  echo "restored $DOMAIN $KEY to ${ORIGINAL:-<absent>}"
  if [ "$code" = 0 ]; then rm -rf "$HOME_DIR"; else echo "kept for its log: $HOME_DIR"; fi
  exit "$code"
}
trap restore EXIT
trap 'exit 130' INT TERM HUP

fail() { echo "FAIL: $*" >&2; exit 1; }
json() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
# `ctl state` exits non-zero when it answers "not managing" (standing down): read it all the same.
state_now() { "$BIN" ctl state || true; }

defaults write "$DOMAIN" "$KEY" -bool false
open -n "$FIXTURE" --env FIXTURE_WINDOWS=2
sleep 2
open -n "$APP" --env STAGE_MANAGER_HOME="$HOME_DIR" --env STAGE_MANAGER_ONLY="$FIX_ID" \
  --env STAGE_MANAGER_NO_PROMPT=1 --env STAGE_MANAGER_TEST=1 --env SM_TEST_TOGGLE_APPLE_SM=1
for _ in $(seq 1 50); do "$BIN" ctl ping >/dev/null 2>&1 && break; sleep 0.2; done
sleep 3
state_now | json 'd["ok"]' | grep -q True || fail "not managing at start"

# Park something: the focused fixture window to a new stage leaves the other parked.
"$BIN" ctl new >/dev/null || fail "ctl new"
sleep 2
PARKED="$(state_now | json 'sum(1 for w in d.get("windows", []) if w.get("parked"))')"
[ "${PARKED:-0}" -ge 1 ] || fail "nothing parked before the toggle"
echo "parked before: $PARKED"
# The stages as Apple's comes on: two of the one app (a fresh start would make one).
STAGES_BEFORE="$(state_now | json 'sum(1 for st in d["stages"] if st["windows"])')"
echo "stages before: $STAGES_BEFORE"

defaults write "$DOMAIN" "$KEY" -bool true
# The app reads the setting once a second; a write from `defaults` reaches it within a few.
for _ in $(seq 1 40); do
  state_now | json 'not d.get("ok") or not any(st["windows"] for st in d.get("stages", []))' | grep -q True && break
  sleep 0.25
done
"$BIN" ctl ping | json 'd["apple_stage_manager_on"]' | grep -q True || fail "the app did not see Apple's Stage Manager on"
# Standing down: no stage holds a window (the fresh controller has its displays already, so
# `ctl state` answers, with nothing in it).
state_now | json 'not d.get("ok") or not any(st["windows"] for st in d.get("stages", []))' | grep -q True || { state_now | head -c 600; echo; fail "still managing while Apple's is on"; }
LEFT="$("$BIN" probe-windows "$FIX_ID" | json 'sum(1 for w in d["windows"] if w["layer"]==0 and w["bounds"]["x"] > d["display"]["w"] - 100)')"
[ "${LEFT:-1}" = "0" ] || fail "$LEFT fixture windows still parked after standing down"
echo "stood down: every window back"

defaults write "$DOMAIN" "$KEY" -bool false
for _ in $(seq 1 40); do
  state_now | json 'd["ok"]' | grep -q True && break
  sleep 0.25
done
state_now | json 'd["ok"]' | grep -q True || fail "not managing again after Apple's went off"
echo "resumed: managing again"
# The stages set aside come back (11-16: they came back as one stage per app).
for _ in $(seq 1 20); do
  STAGES_AFTER="$(state_now | json 'sum(1 for st in d["stages"] if st["windows"])')"
  [ "$STAGES_AFTER" = "$STAGES_BEFORE" ] && break
  sleep 0.3
done
[ "$STAGES_AFTER" = "$STAGES_BEFORE" ] || fail "stages after Apple's went off: $STAGES_AFTER, before: $STAGES_BEFORE (one stage per app: not set aside)"
echo "stages back: $STAGES_AFTER"
[ ! -e "$HOME_DIR/state-stood-down.json" ] || [ -e "$HOME_DIR/state.json" ] || fail "the stages set aside were not taken back"
echo "PASS"
