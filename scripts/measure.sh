#!/bin/bash
# What the app costs, the same way each time (round sm-measure; scripts/measure_stages.py says
# what each scenario measures). Builds and signs the app and the fixtures (CERT_NAME: a
# Developer ID identity keeps the dev copy's Screen Recording and Accessibility grants), puts
# the app in the dev copy, then runs every scenario under each engine in turn, RUNS times,
# and prints the medians and spreads.
#
#   scripts/measure.sh                         # 3 runs of off, corner, spaces; every scenario
#   scripts/measure.sh rest sample             # some scenarios: rest sample switch call launch
#   RUNS=1 ENGINES="corner spaces" scripts/measure.sh switch
#   OUT=/tmp/sm-measure.x scripts/measure.sh   # where measure.jsonl, samples and clocks go
#   SM_CAPTURE=0 OUT=/tmp/sm-nocap scripts/measure.sh rest sample   # captures off: no capture work
#   scripts/measure.sh --summary DIR           # the table again, from DIR/measure.jsonl
#   scripts/measure.sh hidden                  # by name only: a drawing window hidden in its stage and shown
#   SM_SWITCH_WAYS=ctl scripts/measure.sh switch   # switches by `ctl switch` (default "card dock")
#
# scripts/measure_engines.py runs this for the two engines alone, captures off, and writes the
# corner-against-spaces table (PLAN 1.9).
#
# `off` is the app not running: the same fixture windows, the floor. Fixture windows only
# (STAGE_MANAGER_ONLY), a scratch home per run; nothing is measured while another Stage
# Manager runs (quit the installed one first). The measured processes start through
# LaunchServices, so a `nice` on this script never reaches them; run it without one anyway
# (the window samplers it starts keep their pace).
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

if [ "${1:-}" = "--summary" ]; then
  exec python3 scripts/measure_stages.py summary "${2:?usage: $0 --summary DIR}"
fi
RUNS="${RUNS:-3}"
ENGINES="${ENGINES:-off corner spaces}"
OUT="${OUT:-$(mktemp -d /tmp/sm-measure.XXXXXX)}"
mkdir -p "$OUT"

FIX_ID="${SM_FIXTURE_ID:-dev.eugene.stagemanager.fixture.stages}"
SUFFIX="${FIX_ID##*.}"
FIX_NAME="SM Fixture $(tr '[:lower:]' '[:upper:]' <<<"${SUFFIX:0:1}")${SUFFIX:1}"
if [ -z "${SKIP_BUILD:-}" ]; then
  make sign >/dev/null || { echo "build failed" >&2; exit 1; }
fi
# The stage apps (the main fixture, A, B, C) and the call stand-in (<id>.clock, never on
# STAGE_MANAGER_ONLY), rebuilt when missing or older than the fixture's source.
for k in "" a b c clock; do
  if [ -z "$k" ]; then id="$FIX_ID"; app="target/$FIX_NAME.app"
  else id="$FIX_ID.$k"; app="target/$FIX_NAME $(tr '[:lower:]' '[:upper:]' <<<"${k:0:1}")${k:1}.app"; fi
  exe="$app/Contents/MacOS/sm-fixture"
  if [ -x "$exe" ] && [ ! scripts/fixture/main.swift -nt "$exe" ]; then continue; fi
  FIXTURE_ID="$id" FIXTURE_APP="$app" scripts/bundle.sh fixture >/dev/null || exit 1
  scripts/sign.sh "$app" >/dev/null || exit 1
done
scripts/dev-app.sh >/dev/null || exit 1

echo "out: $OUT (runs $RUNS; engines $ENGINES; scenarios ${*:-all})"
code=0
for run in $(seq 1 "$RUNS"); do
  for engine in $ENGINES; do
    python3 scripts/measure_stages.py run --engine "$engine" --run "$run" --out "$OUT" "$@" || code=1
  done
done
echo
python3 scripts/measure_stages.py summary "$OUT"
exit $code
