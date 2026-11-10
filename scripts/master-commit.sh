#!/bin/bash
# Adds one commit to `master`, the branch of releases. `master` shares no history with `dev`:
# each of its commits is one release (or one change to the repository's own pages), made from
# `dev`'s files, so `git log master` reads as the list of releases.
#
#   scripts/master-commit.sh release X.Y.Z "X.Y.Z: what a user notices, in a few words"
#       master's next commit gets dev's files as they are; the message body is CHANGELOG.md's
#       section for X.Y.Z. Run it at step 6 of docs/RELEASING.md, then tag that commit.
#
#   scripts/master-commit.sh repo "Repository: what changed" PATH...
#       master keeps its release files and takes only the given files or folders from dev,
#       such as .github (contributing notes, templates, workflows) or docs/index.html (the
#       info page). GitHub reads those from the default branch, master, so a change to them
#       moves master between releases. The message body comes on stdin: what changed, as
#       CHANGELOG lines ("- ..."), so every commit on master says its scope of work.
#
# Only the local `master` moves; pushing it is a separate step.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"

kind=${1:-}
case "$kind" in
  release)
    version=${2:?version, e.g. 1.8.4}
    subject=${3:?subject line}
    body=$(awk -v v="$version" '$0 ~ "^## "v"( |$)" {on=1; next} on && /^## / {exit} on' \
      <(git show dev:CHANGELOG.md) | sed -e '/./,$!d')
    [ -n "$body" ] || { echo "CHANGELOG.md on dev has no section for $version" >&2; exit 1; }
    tree=$(git rev-parse 'dev^{tree}')
    message=$(printf '%s\n\n%s\n' "$subject" "$body")
    ;;
  repo)
    subject=${2:?subject line}
    shift 2
    [ $# -gt 0 ] || { echo "name the paths to take from dev" >&2; exit 2; }
    index=$(mktemp)
    trap 'rm -f "$index"' EXIT
    export GIT_INDEX_FILE=$index
    git read-tree master
    for path in "$@"; do
      path=${path%/}
      git ls-files -- "$path" | while IFS= read -r file; do
        git update-index --force-remove -- "$file"
      done
      git ls-tree -r --full-tree dev -- "$path" |
        awk -F'\t' '{split($1, m, " "); print m[1] "," m[3] "," $2}' |
        while IFS=, read -r mode sha file; do
          git update-index --add --cacheinfo "$mode,$sha,$file"
        done
    done
    tree=$(git write-tree)
    unset GIT_INDEX_FILE
    [ -t 0 ] && { echo "the message body comes on stdin (CHANGELOG-style lines)" >&2; exit 2; }
    body=$(sed -e '/./,$!d')
    [ -n "$body" ] || { echo "the message body is empty; say what changed" >&2; exit 1; }
    message=$(printf '%s\n\n%s\n' "$subject" "$body")
    ;;
  *)
    sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac

if [ "$tree" = "$(git rev-parse 'master^{tree}')" ]; then
  echo "master already has these files; nothing to commit" >&2
  exit 1
fi

commit=$(printf '%s\n' "$message" | git commit-tree "$tree" -p master)
git update-ref refs/heads/master "$commit" "$(git rev-parse master)"
git log -1 --format='%h %s' master
