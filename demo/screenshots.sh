#!/bin/sh
# Renders the demo into img/dekit-tui.png and img/dekit-up.png at the repo root.
# DEKIT names the dekit binary (default: dekit on PATH).
set -eu

demo=$(cd "$(dirname "$0")" && pwd)

fail() {
  echo "screenshots.sh: $1" >&2
  exit 1
}

command -v python3 >/dev/null || fail "python3 is not installed"
FREEZE=$(command -v freeze) || fail "freeze is not installed: https://github.com/charmbracelet/freeze"
dekit=${DEKIT:-dekit}
DEKIT=$(command -v "$dekit") || fail "dekit not found: $dekit (set DEKIT to the dekit binary)"
case $DEKIT in
  /*) ;;
  *) DEKIT=$PWD/$DEKIT ;;
esac

export DEKIT FREEZE DEMO_STILL=1 XDG_CONFIG_HOME=/nonexistent XDG_DATA_HOME=/nonexistent
cd "$demo"

if ! "$DEKIT" runner status --json | grep -q '"status":"absent"'; then
  fail "a runner for demo/ is already running; stop it first with: dekit runner stop"
fi

trap '"$DEKIT" runner stop >/dev/null 2>&1 || true' EXIT
trap 'exit 1' HUP INT TERM

python3 screenshots.py ../img
"$DEKIT" runner stop >/dev/null
