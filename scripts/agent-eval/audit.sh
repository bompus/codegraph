#!/usr/bin/env bash
# One-shot CodeGraph quality audit:
#   prepare an isolated binary -> ensure corpus repo -> wipe+reindex with that
#   binary -> run with/without A/B.
#
# Usage: audit.sh <version> <repo-name> <repo-url> "<question>" [headless|all]
#   <version>    "local" (build this repo) | "latest" | a version (e.g. 0.7.10)
#   <repo-name>  dir name under the corpus dir
#   <repo-url>   git URL (cloned --depth 1 when the repo dir is missing)
#   [mode]       headless (default) | all (also the interactive tmux arms)
# Env: CORPUS  corpus dir (default: /tmp/codegraph-corpus)
set -euo pipefail

VERSION="${1:?usage: audit.sh <version> <repo-name> <repo-url> \"<question>\" [mode]}"
NAME="${2:?repo-name required}"
URL="${3:?repo-url required}"
Q="${4:?question required}"
MODE="${5:-headless}"

HARNESS="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HARNESS/../.." && pwd)"     # codegraph repo root
CORPUS="${CORPUS:-/tmp/codegraph-corpus}"
REPO="$CORPUS/$NAME"
PKG="@colbymchenry/codegraph"
PREFIX="$(mktemp -d "${TMPDIR:-/tmp}/codegraph-audit.XXXXXX")"
trap 'rm -rf "$PREFIX"' EXIT

echo "==================== CodeGraph audit ===================="
echo "version=$VERSION  repo=$NAME  mode=$MODE  corpus=$CORPUS"
echo

# 1. Prepare the exact binary under test without touching the global install.
if [ "$VERSION" = local ]; then
  echo "→ [1/4] building local dev binary"
  ( cd "$REPO_ROOT" && npm run build )
  CG_BIN="$REPO_ROOT/dist/bin/codegraph.js"
else
  echo "→ [1/4] installing $PKG@$VERSION under isolated prefix $PREFIX"
  npm install --prefix "$PREFIX" --no-save "$PKG@$VERSION"
  CG_BIN="$PREFIX/node_modules/.bin/codegraph"
fi
[ -x "$CG_BIN" ] || { echo "codegraph binary was not created at $CG_BIN"; exit 1; }
ACTUAL="$("$CG_BIN" --version)"
echo "  isolated codegraph: $CG_BIN -> $ACTUAL"

# 2. Ensure the corpus repo exists (clone shallow if missing, reuse if present).
mkdir -p "$CORPUS"
if [ -d "$REPO/.git" ]; then
  echo "→ [2/4] reusing existing checkout: $REPO"
else
  echo "→ [2/4] cloning $URL"
  git clone --depth 1 "$URL" "$REPO" || { echo "git clone failed"; exit 1; }
fi

# 3. Wipe + re-index with THIS version (the index must be built by the same
#    binary that serves it — different versions extract differently).
echo "→ [3/4] wiping .codegraph and re-indexing with $ACTUAL"
rm -rf "$REPO/.codegraph"
( cd "$REPO" && "$CG_BIN" init -i )

# 4. Run the with/without A/B.
echo "→ [4/4] running A/B harness (mode=$MODE)"
CG_BIN="$CG_BIN" bash "$HARNESS/run-all.sh" "$REPO" "$Q" "$MODE"
echo "==================== audit complete ===================="
