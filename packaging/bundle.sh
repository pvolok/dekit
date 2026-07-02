#!/bin/sh
# Stage the install scripts into a release directory and write SHA256SUMS over
# everything install.sh / install.ps1 download. Used by both the canary and the
# tagged-release workflows so the two produce identical checksum manifests.
#
# Usage: packaging/bundle.sh <dir>
set -eu

dir=$1
cp packaging/install/install.sh "$dir/install.sh"
cp packaging/install/install.ps1 "$dir/install.ps1"
(
  cd "$dir"
  sha256sum dekit-* install.sh install.ps1 > SHA256SUMS
  ls -la
)
