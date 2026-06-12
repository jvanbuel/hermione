#!/bin/sh
# Installs the `hermione` terminal recorder.
#
#   curl -fsSL https://raw.githubusercontent.com/jvanbuel/hermione/main/scripts/install-recorder.sh | sh
#
# Downloads a prebuilt binary from GitHub Releases for the host's OS/arch, and
# falls back to building from source with cargo if no asset matches.
set -eu

REPO="jvanbuel/hermione"
VERSION="${HERMIONE_VERSION:-latest}"
BIN_DIR="${HERMIONE_BIN_DIR:-/usr/local/bin}"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *) echo "unsupported arch: $arch" >&2 ;;
esac
target="hermione-${os}-${arch}"

if [ "$VERSION" = "latest" ]; then
  url="https://github.com/${REPO}/releases/latest/download/${target}.tar.gz"
else
  url="https://github.com/${REPO}/releases/download/${VERSION}/${target}.tar.gz"
fi

echo "Installing hermione recorder (${target}) to ${BIN_DIR}..."
tmp="$(mktemp -d)"
if curl -fsSL "$url" -o "$tmp/hermione.tar.gz" 2>/dev/null; then
  tar -xzf "$tmp/hermione.tar.gz" -C "$tmp"
  install -m 0755 "$tmp/hermione" "$BIN_DIR/hermione"
  echo "Installed $("$BIN_DIR/hermione" --version 2>/dev/null || echo hermione)."
elif command -v cargo >/dev/null 2>&1; then
  echo "No prebuilt binary for ${target}; building from source with cargo..."
  cargo install --git "https://github.com/${REPO}" hermione-recorder --bin hermione
else
  echo "No prebuilt binary for ${target} and cargo is not installed." >&2
  rm -rf "$tmp"
  exit 1
fi
rm -rf "$tmp"
