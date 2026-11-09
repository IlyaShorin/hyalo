#!/bin/sh
# ub-mode quality gates: crapper (CRAP < 10), dryer (duplication < 0.8),
# mutator (mutation testing).
#
# The tools live side by side in $UNCLEBOB_TOOLS (default ~/code/unclebob),
# each with its own .venv. Their bundled launchers assume a Unix venv layout
# (.venv/bin), so this script calls each venv's python directly.
#
#   scripts/gates.sh                 crap + dry
#   scripts/gates.sh crap [args]     crapper only
#   scripts/gates.sh dry [args]      dryer only
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

cd "$ROOT"
cmd="${1:-all}"
[ $# -gt 0 ] && shift

case "$cmd" in
  crap)   run crapper --threshold 10 "$@" ;;
  dry)    run dryer --threshold 0.8 "$@" ;;
  mutate) run mutator "$@" ;;
  all)
    run crapper --threshold 10
    run dryer --threshold 0.8
    ;;
  *)
    printf 'usage: %s [crap|dry|mutate|all] [tool args]\n' "$0" >&2
    exit 1
    ;;
esac
