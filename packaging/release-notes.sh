#!/bin/sh
# Print the CHANGELOG.md section of a version, for the GitHub release notes.
# A prerelease such as 1.2.3-rc.1 takes the section of 1.2.3, or the
# Unreleased one while 1.2.3 has none. Fails when there are no notes.
#
# Usage: packaging/release-notes.sh <version>
set -eu

version=${1:?usage: packaging/release-notes.sh <version>}
version=${version#v}
base=${version%%-*}

# The lines under `## <heading>`, up to the next `## `, without the blank
# lines around them.
section() {
  awk -v heading="$1" '
    /^## / {
      if (found) exit
      found = $2 == heading
      next
    }
    !found { next }
    /^[ \t]*$/ {
      if (started) blanks++
      next
    }
    {
      for (; blanks > 0; blanks--) print ""
      started = 1
      print
    }
  ' CHANGELOG.md
}

notes=$(section "$base")
if [ -z "$notes" ] && [ "$version" != "$base" ]; then
  notes=$(section Unreleased)
fi
if [ -z "$notes" ]; then
  printf 'CHANGELOG.md has no notes for %s\n' "$version" >&2
  exit 1
fi
printf '%s\n' "$notes"
