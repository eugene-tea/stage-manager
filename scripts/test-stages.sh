#!/bin/bash
# Scripted checks of milestones 3, 4 and 6 (see scripts/test_stages.py): builds the app and a
# private fixture ("SM Fixture Stages.app", bundle ID dev.eugene.stagemanager.fixture.stages,
# so no other fixture run is disturbed), then runs the phases.
#
#   scripts/test-stages.sh                 # every phase: m3 safety m4 m6
#   scripts/test-stages.sh m3 safety       # some phases
#   SKIP_BUILD=1 TEST_HOME=/tmp/sm scripts/test-stages.sh
#
# Every app run uses STAGE_MANAGER_HOME=$TEST_HOME (default: a new /tmp/sm-test.* dir) and
# STAGE_MANAGER_ONLY=<the fixture>; only fixture windows ever move. The fixture app comes to
# the front during switches; the app that was frontmost is activated again at the end.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

if [ -z "${SKIP_BUILD:-}" ]; then
  make sign >/dev/null || { echo "build failed" >&2; exit 1; }
  FIXTURE_ID=dev.eugene.stagemanager.fixture.stages FIXTURE_APP="target/SM Fixture Stages.app" \
    scripts/bundle.sh fixture >/dev/null || exit 1
  scripts/sign.sh "target/SM Fixture Stages.app" >/dev/null || exit 1
fi
exec python3 scripts/test_stages.py "$@"
