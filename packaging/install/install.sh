#!/bin/sh
# Installs dekit. `dekit update` runs this same script.
#
#   DEKIT_VERSION       latest (default), canary, or a version like 1.2.3
#   DEKIT_INSTALL_DIR   where the binary goes (default: ~/.local/bin)
#   DEKIT_RELEASES_URL  a mirror of https://github.com/pvolok/dekit/releases
set -eu

err() {
  printf 'dekit: %s\n' "$1" >&2
  exit 1
}

has() {
  command -v "$1" >/dev/null 2>&1
}

download() {
  if has curl; then
    curl -fsSL "$1" -o "$2"
  elif has wget; then
    wget -qO "$2" "$1"
  else
    err "missing required command: curl or wget"
  fi || err "could not download $1"
}

sha256() {
  if has sha256sum; then
    sha256sum "$1" | awk '{ print $1 }'
  elif has shasum; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    err "missing required command: sha256sum or shasum"
  fi
}

# Everything runs from main, called on the last line, so a download that
# was cut off runs nothing.
main() {
  releases=${DEKIT_RELEASES_URL:-https://github.com/pvolok/dekit/releases}
  version=${DEKIT_VERSION:-latest}
  install_dir=${DEKIT_INSTALL_DIR:-"$HOME/.local/bin"}

  case "$(uname -s)" in
    Darwin) os=apple-darwin ;;
    Linux) os=unknown-linux-musl ;;
    *) err "unsupported OS: $(uname -s)" ;;
  esac

  case "$(uname -m)" in
    x86_64 | amd64) cpu=x86_64 ;;
    arm64 | aarch64) cpu=aarch64 ;;
    *) err "unsupported CPU: $(uname -m)" ;;
  esac
  # A shell running under Rosetta reports x86_64 on Apple silicon.
  if [ "$os" = apple-darwin ] &&
    [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
    cpu=aarch64
  fi

  asset="dekit-$cpu-$os.tar.gz"
  case "$version" in
    latest) url="$releases/latest/download" ;;
    canary | v*) url="$releases/download/$version" ;;
    *) url="$releases/download/v$version" ;;
  esac

  has tar || err "missing required command: tar"

  tmp_dir=$(mktemp -d 2>/dev/null || mktemp -d -t dekit)
  new="$install_dir/.dekit-new-$$"
  trap 'rm -rf "$tmp_dir" "$new"' EXIT
  trap 'exit 1' INT TERM

  download "$url/$asset" "$tmp_dir/$asset"
  download "$url/SHA256SUMS" "$tmp_dir/SHA256SUMS"

  expected=$(awk -v asset="$asset" \
    '$2 == asset || $2 == "*" asset { print $1; exit }' "$tmp_dir/SHA256SUMS")
  [ -n "$expected" ] || err "SHA256SUMS has no checksum for $asset"
  actual=$(sha256 "$tmp_dir/$asset")
  [ "$actual" = "$expected" ] || err "checksum mismatch for $asset"

  tar -xzf "$tmp_dir/$asset" -C "$tmp_dir"
  installed=$("$tmp_dir/dekit" --version) ||
    err "the downloaded binary does not run on this machine"

  # Rename into place: the binary is never half written, and a running
  # dekit keeps the file it started from.
  mkdir -p "$install_dir"
  cp "$tmp_dir/dekit" "$new"
  chmod 755 "$new"
  mv -f "$new" "$install_dir/dekit"

  printf '%s installed to %s\n' "$installed" "$install_dir/dekit"

  case ":$PATH:" in
    *:"$install_dir":*) ;;
    *)
      printf 'dekit: %s is not on PATH; add this to your shell profile:\n' "$install_dir" >&2
      printf '  export PATH="%s:$PATH"\n' "$install_dir" >&2
      ;;
  esac
}

main
