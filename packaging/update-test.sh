#!/bin/sh
# Installs one published release and updates it to another while a runner
# is running. The runner must end on the new binary with its task's
# process untouched. Run from the canary workflow, so a commit that an
# installed dekit cannot update to is never tagged.
#
# Usage: packaging/update-test.sh <from> <to>    (latest, canary, 1.2.3)
set -eu

from=$1
to=$2
releases=${DEKIT_RELEASES_URL:-https://github.com/pvolok/dekit/releases}

release_url() {
  case "$1" in
    latest) echo "$releases/latest/download" ;;
    canary | v*) echo "$releases/download/$1" ;;
    *) echo "$releases/download/v$1" ;;
  esac
}

fail() {
  printf 'update-test: %s\n' "$1" >&2
  exit 1
}

# Short, because the runner's socket path has a length limit.
dir=$(mktemp -d /tmp/dk.XXXXXX)
export DEKIT_INSTALL_DIR="$dir/bin"
export XDG_RUNTIME_DIR="$dir/run" XDG_DATA_HOME="$dir/data" XDG_CONFIG_HOME="$dir/config"
mkdir -p "$dir/project" "$XDG_RUNTIME_DIR"
cd "$dir/project"
dekit="$DEKIT_INSTALL_DIR/dekit"
trap '"$dekit" runner stop >/dev/null 2>&1; rm -rf "$dir"' EXIT

curl -fsSL "$(release_url "$from")/install.sh" -o "$dir/install.sh"
DEKIT_VERSION=$from sh "$dir/install.sh"

echo 'tasks: {sleeper: {cmd: ["sh", "-c", "echo pid $$; exec sleep 1000"], autostart: true}}' > dekit.yaml
"$dekit" up
pid=
for _ in 1 2 3 4 5 6 7 8 9 10; do
  pid=$("$dekit" screen sleeper | sed -n 's/^pid \([0-9]*\).*/\1/p')
  [ -z "$pid" ] || break
  sleep 1
done
[ -n "$pid" ] || fail "the task printed no pid"
runner_pid=$("$dekit" --json runner status | jq -r .pid)

DEKIT_INSTALL_URL="$(release_url "$to")/install.sh" "$dekit" update "$to"
# Two builds of one version count as the same to `update`. Switch anyway,
# so the old runner always hands over to the new binary here.
"$dekit" runner upgrade

version=$("$dekit" --version | awk '{ print $2 }')
status=$("$dekit" --json runner status)
[ "$(echo "$status" | jq -r .status)" = running ] || fail "the runner is not running: $status"
[ "$(echo "$status" | jq -r .version)" = "$version" ] || fail "the runner is not on $version: $status"
[ "$(echo "$status" | jq -r .pid)" = "$runner_pid" ] || fail "the runner was restarted: $status"
kill -0 "$pid" 2>/dev/null || fail "the task's process $pid is gone"
"$dekit" ls | grep -q sleeper || fail "the task is gone"

"$dekit" down
echo "update-test: $from -> $to ($version) ok"
