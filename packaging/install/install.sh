#!/bin/sh
set -eu

repo=${DEKIT_REPO:-pvolok/dekit}
version=${DEKIT_VERSION:-latest}
install_dir=${DEKIT_INSTALL_DIR:-"$HOME/.local/bin"}

err() {
  printf 'dekit: %s\n' "$1" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || err "missing required command: $1"
}

case "$(uname -s)" in
  Darwin) os=darwin ;;
  Linux) os=linux ;;
  *) err "unsupported OS: $(uname -s)" ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) arch=x64 ;;
  arm64 | aarch64) arch=arm64 ;;
  *) err "unsupported CPU: $(uname -m)" ;;
esac

case "$os-$arch" in
  darwin-arm64) target=aarch64-apple-darwin ;;
  darwin-x64) target=x86_64-apple-darwin ;;
  linux-arm64) target=aarch64-unknown-linux-musl ;;
  linux-x64) target=x86_64-unknown-linux-musl ;;
  *) err "unsupported platform: $os-$arch" ;;
esac

asset="dekit-$target.tar.gz"
case "$version" in
  latest | canary | v*) release_version=$version ;;
  *) release_version="v$version" ;;
esac

if [ "$release_version" = "latest" ]; then
  base_url="https://github.com/$repo/releases/latest/download"
else
  base_url="https://github.com/$repo/releases/download/$release_version"
fi

tmp_dir=$(mktemp -d 2>/dev/null || mktemp -d -t dekit)
trap 'rm -rf "$tmp_dir"' EXIT INT TERM

download() {
  url=$1
  dest=$2
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$dest"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$dest" "$url"
  else
    err "missing required command: curl or wget"
  fi
}

verify_checksum() {
  sums=$tmp_dir/SHA256SUMS
  if ! download "$base_url/SHA256SUMS" "$sums"; then
    printf 'dekit: warning: could not download SHA256SUMS; skipping checksum verification\n' >&2
    return
  fi

  expected=$(awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1; exit }' "$sums")
  if [ -z "$expected" ]; then
    printf 'dekit: warning: no checksum found for %s; skipping checksum verification\n' "$asset" >&2
    return
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp_dir/$asset" | awk '{ print $1 }')
  elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp_dir/$asset" | awk '{ print $1 }')
  else
    printf 'dekit: warning: missing sha256sum or shasum; skipping checksum verification\n' >&2
    return
  fi

  [ "$actual" = "$expected" ] || err "checksum mismatch for $asset"
}

need tar
mkdir -p "$install_dir"
download "$base_url/$asset" "$tmp_dir/$asset"
verify_checksum
tar -xzf "$tmp_dir/$asset" -C "$tmp_dir"
install -m 755 "$tmp_dir/dekit" "$install_dir/dekit"

case ":$PATH:" in
  *:"$install_dir":*) ;;
  *)
    printf 'dekit: installed to %s, but that directory is not on PATH\n' "$install_dir" >&2
    printf 'dekit: add this to your shell profile: export PATH="%s:$PATH"\n' "$install_dir" >&2
    ;;
esac

printf 'dekit installed to %s\n' "$install_dir/dekit"
