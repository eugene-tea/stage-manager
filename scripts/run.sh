#!/bin/bash
# Launches the dev copy of the app (scripts/dev-app.sh) through LaunchServices (so TCC checks the bundle, not
# the terminal) and streams its log. Ctrl-C quits the app: `ctl quit` first, SIGTERM as a
# fallback.
#
# Passed through to the app when set: RUST_LOG (default info,stage_manager=debug),
# STAGE_MANAGER_NO_PROMPT, STAGE_MANAGER_ONLY, STAGE_MANAGER_HOME (config, state, socket and
# log in that directory), STAGE_MANAGER_TEST (test-only ctl commands).
set -uo pipefail
cd "$(dirname "$0")/.."

APP="$(scripts/dev-app.sh)" || exit 1
BIN="$APP/Contents/MacOS/stage-manager"
if [ -n "${STAGE_MANAGER_HOME:-}" ]; then
  mkdir -p "$STAGE_MANAGER_HOME"
  STAGE_MANAGER_HOME="$(cd "$STAGE_MANAGER_HOME" && pwd)"
  export STAGE_MANAGER_HOME
  LOG="$STAGE_MANAGER_HOME/stage-manager.log"
else
  LOG="$HOME/Library/Logs/Stage Manager/stage-manager.log"
fi

if "$BIN" ctl ping >/dev/null 2>&1; then
  echo "Stage Manager is already running; quit it first (\"$BIN\" ctl quit)." >&2
  exit 1
fi

mkdir -p "$(dirname "$LOG")"
: > "$LOG"

ENV_ARGS=(--env "RUST_LOG=${RUST_LOG:-info,stage_manager=debug}")
for var in STAGE_MANAGER_NO_PROMPT STAGE_MANAGER_ONLY STAGE_MANAGER_HOME STAGE_MANAGER_TEST; do
  if [ -n "${!var+x}" ]; then ENV_ARGS+=(--env "$var=${!var}"); fi
done

open -n "$APP" "${ENV_ARGS[@]}" || exit 1

# The app's pid: from `ctl ping`, else from the process table.
PID=""
for _ in $(seq 1 50); do
  if REPLY="$("$BIN" ctl ping 2>/dev/null)"; then
    PID="$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' <<<"$REPLY")"
    break
  fi
  sleep 0.1
done
[ -n "$PID" ] || PID="$(pgrep -n -f "Stage Manager.app/Contents/MacOS/stage-manager$")"
if [ -z "$PID" ]; then
  echo "Stage Manager did not start; log follows." >&2
  cat "$LOG" >&2
  exit 1
fi

tail -n +1 -F "$LOG" &
TAIL=$!

quit_app() {
  "$BIN" ctl quit >/dev/null 2>&1
  for _ in $(seq 1 30); do
    kill -0 "$PID" 2>/dev/null || return 0
    sleep 0.1
  done
  echo "Stage Manager did not quit over ctl; sending SIGTERM" >&2
  kill -TERM "$PID" 2>/dev/null
}

trap 'quit_app; kill "$TAIL" 2>/dev/null; exit 0' INT TERM

while kill -0 "$PID" 2>/dev/null; do sleep 0.5; done
kill "$TAIL" 2>/dev/null
echo "Stage Manager exited."
