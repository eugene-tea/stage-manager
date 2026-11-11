#!/bin/bash
# Live check of an update installed at the user's Quit (the owner's go 11-09), with a local fake
# release: a test build (--features update-test) of this checkout runs from a scratch folder,
# and a copy of it relabelled 99.0.0 sits on a local disk image (a locally signed app trusts
# its own designated requirement in test builds; STAGE_MANAGER_UPDATE_DMG).
#
#   1. The update is readied as an automatic install, then `ctl quit` (not the user's Quit):
#      nothing is installed, no helper starts, the staged copy is left behind.
#   2. The next launch keeps that copy (automatic installs are on) and says it checks again
#      within the first minute.
#   3. Readied again from the kept copy, verified again and not downloaded, then the menu's
#      Quit (`ctl debug-menu-quit`): the helper replaces the scratch copy with 99.0.0 and
#      opens nothing.
#
# Manages no app (STAGE_MANAGER_ONLY names a bundle ID nothing has), in a scratch home, no
# login item; never touches ~/Applications, the dev copy or the installed app. Refuses to run
# beside a running Stage Manager. Needs the local signing identity (scripts/make-cert.sh).
# An automatic install also goes by itself once the Mac had no input for 120 s: run it while
# at the Mac (step 1 says so if that happened).
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

NEW_VERSION="99.0.0"
SCRATCH="$(mktemp -d /tmp/sm-update-quit.XXXXXX)"
HOME_DIR="$SCRATCH/home"
APP="$SCRATCH/apps/Stage Manager.app"
BIN="$APP/Contents/MacOS/stage-manager"
WORK="$SCRATCH/apps/.Stage Manager update"
NEW_DIR="$SCRATCH/new"
DMG="$SCRATCH/Stage-Manager.dmg"
mkdir -p "$HOME_DIR" "$SCRATCH/apps" "$NEW_DIR"
export STAGE_MANAGER_HOME="$HOME_DIR"

PASS=0
FAIL=0
ok() { echo "  ok: $*"; PASS=$((PASS + 1)); }
bad() { echo "  FAIL: $*" >&2; FAIL=$((FAIL + 1)); }
check() { if eval "$1"; then ok "$2"; else bad "$2"; fi; }
log_text() { cat "$HOME_DIR"/*.log 2>/dev/null; }
version_of() { /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$1/Contents/Info.plist" 2>/dev/null; }
app_pids() { pgrep -f "^$BIN( -psn_[^ ]*)?\$" || true; }

cleanup() {
  local code=$?
  if [ -n "$(app_pids)" ]; then "$BIN" ctl quit >/dev/null 2>&1 || true; sleep 3; fi
  pkill -f "$SCRATCH/apps/" 2>/dev/null || true
  rm -rf "$SCRATCH"
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

# The installed app (the default socket) must not run beside a second copy.
for running in "$HOME/Applications/Stage Manager.app" "/Applications/Stage Manager.app" \
  "$HOME/Library/Application Support/Stage Manager Dev/Stage Manager.app"; do
  if [ -x "$running/Contents/MacOS/stage-manager" ] && env -u STAGE_MANAGER_HOME "$running/Contents/MacOS/stage-manager" ctl ping >/dev/null 2>&1; then
    echo "error: a Stage Manager is running; quit it first" >&2
    exit 1
  fi
done

echo "-- building a test copy (--features update-test) and a fake $NEW_VERSION release"
CARGO_TARGET_DIR=target/update-test nice -n 18 cargo build --release --features update-test || exit 1
SM_APP="$APP" SM_BIN=target/update-test/release/stage-manager scripts/bundle.sh app || exit 1
scripts/sign.sh "$APP" || exit 1
OLD_VERSION="$(version_of "$APP")"
ditto "$APP" "$NEW_DIR/Stage Manager.app"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $NEW_VERSION" "$NEW_DIR/Stage Manager.app/Contents/Info.plist"
scripts/sign.sh "$NEW_DIR/Stage Manager.app" || exit 1
hdiutil create -quiet -volname "Stage Manager" -srcfolder "$NEW_DIR" -fs HFS+ -format UDZO -ov "$DMG" || exit 1
cat > "$HOME_DIR/config.toml" <<'TOML'
[app]
launch_at_login = false
check_for_updates = true
install_updates = true
TOML

start_app() {
  local args=(open -n "$APP")
  local k
  for k in "STAGE_MANAGER_HOME=$HOME_DIR" "STAGE_MANAGER_ONLY=dev.eugene.stagemanager.nothing" \
    "STAGE_MANAGER_NO_PROMPT=1" "STAGE_MANAGER_NO_CAPTURE=1" "STAGE_MANAGER_NO_LOGIN_ITEM=1" \
    "STAGE_MANAGER_TEST=1" "STAGE_MANAGER_UPDATE_VERSION=$NEW_VERSION" "STAGE_MANAGER_UPDATE_DMG=$DMG" \
    "STAGE_MANAGER_UPDATE_AUTO=1" "RUST_LOG=info,stage_manager=debug"; do
    args+=(--env "$k")
  done
  "${args[@]}" || return 1
  local i
  for i in $(seq 1 60); do
    "$BIN" ctl ping >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  return 1
}

wait_log() { # count pattern seconds
  local i
  for i in $(seq 1 $(($3 * 2))); do
    [ "$(log_text | grep -c -- "$2")" -ge "$1" ] && return 0
    sleep 0.5
  done
  return 1
}

wait_gone() {
  local i
  for i in $(seq 1 $(($1 * 2))); do
    [ -z "$(app_pids)" ] && return 0
    sleep 0.5
  done
  return 1
}

echo "-- 1. a quit that is not the user's (ctl quit) installs nothing"
start_app || { bad "the test copy did not start"; exit 1; }
"$BIN" ctl debug-update >/dev/null
check 'wait_log 1 "update: ready to install" 120' "the fake $NEW_VERSION is downloaded and verified"
sleep 2
if log_text | grep -q "update: installing; quitting first"; then
  bad "the Mac had no input for 120 s, so the automatic install went at once: run again while at the Mac"
  exit 1
fi
"$BIN" ctl quit >/dev/null
check 'wait_gone 30' "the app quit"
check 'log_text | grep -q "does not install at this quit"' "the ready update did not install at ctl quit"
check '! log_text | grep -q "installer helper started"' "no installer helper started"
check '[ "$(version_of "$APP")" = "$OLD_VERSION" ]' "the copy is still $OLD_VERSION"
check '[ -d "$WORK" ]' "the staged copy is left behind"

echo "-- 2. the next launch keeps it and checks again early"
start_app || { bad "the test copy did not start again"; exit 1; }
check 'wait_log 1 "an earlier run.s downloaded update stays beside the app" 20' "the left-behind copy is kept"
check 'wait_log 1 "checking again within the first minute" 20' "an early check is owed (test runs make no automatic checks)"
check '[ -d "$WORK/Stage Manager.app" ]' "the staged copy is still there"
check '! log_text | grep -q "removed a working folder"' "nothing was removed"

echo "-- 3. the user's Quit installs it, and nothing reopens"
"$BIN" ctl debug-update >/dev/null
check 'wait_log 2 "update: ready to install" 120' "ready again"
check 'log_text | grep -q "the copy an earlier run staged is verified again; nothing is downloaded"' "the kept copy was verified again, not downloaded"
check '[ "$(log_text | grep -c "update: downloaded; verifying")" -eq 1 ]' "one download in all"
sleep 2
if log_text | grep -q "update: installing; quitting first"; then
  bad "the Mac had no input for 120 s, so the automatic install went at once: run again while at the Mac"
  exit 1
fi
"$BIN" ctl debug-menu-quit >/dev/null
check 'wait_gone 30' "the app quit"
check 'log_text | grep -q "installing at the user.s Quit"' "the user's Quit installs the update"
check 'wait_log 1 "updater: installed" 120' "the helper installed it"
check '[ "$(version_of "$APP")" = "$NEW_VERSION" ]' "the copy is now $NEW_VERSION"
check '[ ! -d "$WORK" ]' "the working folder is gone"
sleep 3
check '[ -z "$(app_pids)" ]' "nothing reopened"
check '! log_text | grep -q "updater: the app could not be opened"' "no reopen by bundle ID"

echo "-- $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
