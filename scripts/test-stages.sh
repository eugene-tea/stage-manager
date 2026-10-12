#!/bin/bash
# Scripted checks (see scripts/test_stages.py): builds the app and private fixtures ("SM
# Fixture Stages.app", bundle ID dev.eugene.stagemanager.fixture.stages, plus "SM Fixture
# Stages A/B/C.app" with IDs <id>.a/.b/.c for the native phase; SM_FIXTURE_ID picks another
# base ID so no other fixture run is disturbed), then runs the phases.
#
#   scripts/test-stages.sh                 # every phase: m3 safety m4 m6 strip native visible parity
#   scripts/test-stages.sh m3 safety       # some phases
#   SKIP_BUILD=1 TEST_HOME=/tmp/sm scripts/test-stages.sh
#
# Every app run uses STAGE_MANAGER_HOME=$TEST_HOME (default: a new /tmp/sm-test.* dir) and
# STAGE_MANAGER_ONLY=<the fixtures>; only fixture windows ever move. The fixture app comes to
# the front during switches; the app that was frontmost is activated again at the end.
# No phase runs while the owner's installed app (~/Applications/Stage Manager.app) is up;
# SM_SCREENSHOTS=<dir> is where the visible phase saves its strip screenshots.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

FIX_ID="${SM_FIXTURE_ID:-dev.eugene.stagemanager.fixture.stages}"
SUFFIX="${FIX_ID##*.}"
FIX_NAME="SM Fixture $(tr '[:lower:]' '[:upper:]' <<<"${SUFFIX:0:1}")${SUFFIX:1}"
if [ -z "${SKIP_BUILD:-}" ]; then
  make sign >/dev/null || { echo "build failed" >&2; exit 1; }
  # The main fixture, plus three apps for the native phase (<id>.a, .b, .c).
  for k in "" a b c; do
    if [ -z "$k" ]; then id="$FIX_ID"; app="target/$FIX_NAME.app"
    else id="$FIX_ID.$k"; app="target/$FIX_NAME $(tr '[:lower:]' '[:upper:]' <<<"$k").app"; fi
    FIXTURE_ID="$id" FIXTURE_APP="$app" scripts/bundle.sh fixture >/dev/null || exit 1
    scripts/sign.sh "$app" >/dev/null || exit 1
  done
fi
exec python3 scripts/test_stages.py "$@"
