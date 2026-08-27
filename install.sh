#!/bin/sh
# e installer: fetch the latest release binary for this platform, verify its
# checksum, install to ~/.local/bin (override with E_INSTALL_DIR).
#   curl -fsSL https://raw.githubusercontent.com/intuitumxyz/e/main/install.sh | sh
set -eu

repo="intuitumxyz/e"
dir="${E_INSTALL_DIR:-$HOME/.local/bin}"

os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin)
    case "$arch" in
      arm64)  target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) echo "unsupported macOS architecture: $arch" >&2; exit 1 ;;
    esac ;;
  Linux)
    case "$arch" in
      aarch64|arm64) target="aarch64-unknown-linux-gnu" ;;
      x86_64)        target="x86_64-unknown-linux-gnu" ;;
      *) echo "unsupported Linux architecture: $arch" >&2; exit 1 ;;
    esac ;;
  *) echo "unsupported platform: $os" >&2; exit 1 ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# E_RELEASE_BASE is an internal release-smoke seam: production installs leave
# it unset; CI points it at the just-built local artifacts.
base="${E_RELEASE_BASE:-https://github.com/$repo/releases/latest/download}"

curl -fsSL -o "$tmp/e.tar.gz" "$base/e-$target.tar.gz" || {
  echo "no release published yet — install.sh works once the first release exists" >&2
  echo "build from source: cargo install --git https://github.com/intuitumxyz/e" >&2
  exit 1
}
curl -fsSL -o "$tmp/checksums.txt" "$base/checksums.txt"

cd "$tmp"
expected=$(grep " e-$target.tar.gz$" checksums.txt | cut -d' ' -f1)
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum e.tar.gz | cut -d' ' -f1)
else
  actual=$(shasum -a 256 e.tar.gz | cut -d' ' -f1)
fi
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
  echo "checksum mismatch — refusing to install" >&2
  exit 1
fi

tar xzf e.tar.gz
mkdir -p "$dir"
install -m 755 e "$dir/e"

echo "installed $("$dir/e" --version) to $dir/e"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "note: $dir is not on your PATH" ;;
esac
