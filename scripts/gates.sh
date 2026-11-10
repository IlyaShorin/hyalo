#!/bin/sh
# ub-mode quality gates: crapper (CRAP < 10), dryer (duplication < 0.8),
# mutator (mutation testing).
#
# The tools live side by side in $UNCLEBOB_TOOLS (default ~/code/unclebob),
# each with its own .venv. Their bundled launchers assume a Unix venv layout
# (.venv/bin), so this script calls each venv's python directly.
#
#   scripts/gates.sh                 crap + dry over crates/ (exit 2 when either fails)
#   scripts/gates.sh crap [args]     crapper only (given no args, over crates/)
#   scripts/gates.sh dry [args]      dryer only (given no args, over crates/)
#   scripts/gates.sh mutate [args]   mutator (slow; try --changed)
set -eu

TOOLS="${UNCLEBOB_TOOLS:-$HOME/code/unclebob}"
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
# The tools print non-ASCII; a Windows console code page (cp1251, cp866)
# makes Python crash on that output.
export PYTHONUTF8=1

tool_python() {
  for p in "$TOOLS/$1/.venv/Scripts/python.exe" "$TOOLS/$1/.venv/bin/python"; do
    if [ -x "$p" ]; then
      printf '%s\n' "$p"
      return 0
    fi
  done
  printf 'gates: no venv for %s in %s\n' "$1" "$TOOLS" >&2
  printf '       cd %s/%s && python -m venv .venv && .venv/Scripts/python -m pip install -e .\n' "$TOOLS" "$1" >&2
  return 1
}

run() {
  tool=$1
  shift
  py=$(tool_python "$tool") || exit 1
  "$py" -m "$tool" --root "$ROOT" "$@"
}

# What the gates look at when given no paths: the workspace's crates.
# Sessions that work beside this one keep git worktrees of the whole
# repository under .claude/, and the tools would take every function
# there for a copy of its own (and build and test each worktree again).
SOURCES=crates

# ub-mode wants every function under CRAP 10; crapper fails only when the
# worst score is above its threshold, so the threshold sits just below 10.
crap() {
  [ $# -gt 0 ] || set -- "$SOURCES"
  run crapper --threshold 9.999 "$@"
}

# dryer reports and exits 0 either way; the gate fails on any candidate.
dry() {
  [ $# -gt 0 ] || set -- "$SOURCES"
  report=$(run dryer --threshold 0.8 "$@")
  printf '%s\n' "$report"
  case "$report" in
    *"No duplicate candidates found."*) return 0 ;;
  esac
  printf 'gates: duplicate candidates at similarity 0.8 or above\n' >&2
  return 2
}

cd "$ROOT"
cmd="${1:-all}"
[ $# -gt 0 ] && shift

case "$cmd" in
  crap)   crap "$@" ;;
  dry)    dry "$@" ;;
  mutate) run mutator "$@" ;;
  all)
    crap
    dry
    ;;
  *)
    printf 'usage: %s [crap|dry|mutate|all] [tool args]\n' "$0" >&2
    exit 1
    ;;
esac
