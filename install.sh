#!/bin/sh
# Install the weft binary from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/d-roak/weft/main/install.sh | sh
#
# Env: WEFT_VERSION (tag, default: latest), WEFT_INSTALL_DIR (default: ~/.local/bin)
set -eu

REPO="d-roak/weft"
# The tarball ships the node CLI plus the relay server.
BINS="weft weft-relay"
INSTALL_DIR="${WEFT_INSTALL_DIR:-$HOME/.local/bin}"

# Map uname -> release target triple.
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux) os_t="unknown-linux-gnu" ;;
  Darwin) os_t="apple-darwin" ;;
  *) echo "weft: unsupported OS '$os' (Linux and macOS only)" >&2; exit 1 ;;
esac
case "$arch" in
  x86_64|amd64) arch_t="x86_64" ;;
  aarch64|arm64) arch_t="aarch64" ;;
  *) echo "weft: unsupported architecture '$arch'" >&2; exit 1 ;;
esac
target="${arch_t}-${os_t}"

# Resolve version (default: latest release tag).
version="${WEFT_VERSION:-}"
if [ -z "$version" ]; then
  version="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | cut -d'"' -f4)"
fi
if [ -z "$version" ]; then
  echo "weft: could not determine latest version; set WEFT_VERSION" >&2
  exit 1
fi

url="https://github.com/${REPO}/releases/download/${version}/weft-${target}.tar.gz"
echo "weft: downloading ${version} (${target})"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
if ! curl -fsSL "$url" -o "$tmp/weft.tar.gz"; then
  echo "weft: download failed — no build for ${target} in ${version}?" >&2
  echo "      $url" >&2
  exit 1
fi
tar -xzf "$tmp/weft.tar.gz" -C "$tmp"

mkdir -p "$INSTALL_DIR"
for bin in $BINS; do
  # Older releases shipped only `weft`; skip anything not in the tarball.
  if [ -f "$tmp/$bin" ]; then
    install -m 0755 "$tmp/$bin" "$INSTALL_DIR/$bin"
    echo "weft: installed to ${INSTALL_DIR}/${bin}"
  fi
done

# PATH hint if needed.
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "weft: add to your shell profile: export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

"$INSTALL_DIR/weft" --version 2>/dev/null || true
