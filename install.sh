#!/bin/sh
# e installer: fetch the latest release binary for this platform, verify its
# checksum, install to ~/.local/bin (override with E_INSTALL_DIR).
#   curl -fsSL https://e.intuitum.sh/install.sh | sh
set -eu

repo="intuitums/e"
dir="${E_INSTALL_DIR:-$HOME/.local/bin}"

# Explicit channel/version selection never changes the default stable installation.
channel=stable
version=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --channel) channel=${2:?channel required}; shift 2 ;;
    --version) version=${2:?version required}; shift 2 ;;
    *) echo 'usage: install.sh [--channel stable|beta] [--version X.Y.Z]' >&2; exit 2 ;;
  esac
done
case "$channel" in
  stable) command=e ;;
  beta) command=e-beta; repo=intuitums/e-beta ;;
  dev)
    # Local release qualification still tests the freshly built dev binary.
    if [ -n "${E_RELEASE_BASE:-}" ]; then command=e-dev
    else echo 'Dev builds use npm install -g @intuitums/e@dev or bun add -g @intuitums/e@dev' >&2; exit 2; fi ;;
  *) echo 'unknown channel' >&2; exit 2 ;; esac
if [ -z "$version" ] && [ "$channel" != stable ]; then
  # Keep the previous beta channel usable until its successor is published.
  status=$(curl -sSL -w '\n%{http_code}' "https://github.com/$repo/releases/latest/download/version.txt")
  case "$status" in
    *200) version=$(printf '%s\n' "$status" | sed '$d') ;;
    *404) version=$(curl -fsSL https://github.com/intuitums/e/releases/download/channel-beta/version.txt); repo=intuitums/e ;;
    *) echo 'Could not resolve the beta version' >&2; exit 1 ;;
  esac
fi
if [ -n "$version" ]; then
  version=${version#v}
  case "$channel" in
    stable) pattern='^[0-9]+\.[0-9]+\.[0-9]+$' ;;
    *) pattern="^[0-9]+\.[0-9]+\.[0-9]+-$channel\.[0-9]+\.g[a-f0-9]{12}$" ;;
  esac
  printf '%s\n' "$version" | grep -Eq "$pattern" || { echo 'version does not match channel' >&2; exit 2; }
fi

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

# The Linux binaries are linked against the glibc of their build image, so a
# system below it cannot run them at all. Refuse here, where the reason is
# nameable, instead of letting the linker fail after the download. The floor is
# overridable so the refusal itself is testable on a compliant host.
if [ "$os" = Linux ]; then
  required=${E_INSTALL_GLIBC:-2.39}
  present=$(ldd --version 2>/dev/null | head -1 | sed -n 's/.*[^0-9]\([0-9][0-9]*\.[0-9][0-9]*\)$/\1/p')
  if [ -n "$present" ] && [ "$(printf '%s\n%s\n' "$required" "$present" | sort -V | head -1)" != "$required" ]; then
    echo "e's Linux binaries need glibc $required or newer; this system has $present." >&2
    echo 'Ubuntu 24.04+, Debian 13+, Fedora 40+, and RHEL 10+ have it. Otherwise run the published image,' >&2
    echo 'which carries its own runtime: docker run --rm --entrypoint e ghcr.io/intuitums/e-slack:latest --version' >&2
    exit 1
  fi
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# E_RELEASE_BASE is an internal release-smoke seam: production installs leave
# it unset; CI points it at the just-built local artifacts.
base="https://github.com/$repo/releases/latest/download"
if [ -n "$version" ]; then base="https://github.com/$repo/releases/download/v$version"; fi
base=${E_RELEASE_BASE:-$base}

# Pinned beta archives published before the repository split remain in the source repository.
downloaded=false
if [ "$channel" = beta ] && [ -z "${E_RELEASE_BASE:-}" ]; then
  status=$(curl -sSL -o "$tmp/e.tar.gz" -w '%{http_code}' "$base/e-$target.tar.gz")
  case "$status" in
    200) downloaded=true ;;
    404) base="https://github.com/intuitums/e/releases/download/v$version" ;;
    *) echo 'Could not download the beta archive' >&2; exit 1 ;;
  esac
fi
[ "$downloaded" = true ] || curl -fsSL -o "$tmp/e.tar.gz" "$base/e-$target.tar.gz" || {
  echo "no release published yet — install.sh works once the first release exists" >&2
  echo "build from source: cargo install --git https://github.com/intuitums/e" >&2
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

tar xzf e.tar.gz e
[ -f e ] && [ ! -L e ] || { echo "invalid executable" >&2; exit 1; }
if [ -n "$version" ] && [ "$(./e --version)" != "e $version" ]; then
  echo "archive identity does not match requested version" >&2
  exit 1
fi
mkdir -p "$dir"
# Refuse to replace package-owned executables, including symlinks into their stores.
if [ -L "$dir/$command" ] || [ -e "$dir/.e-install-method" ]; then
  echo "destination is package-managed; choose another E_INSTALL_DIR" >&2
  exit 1
fi
install -m 755 e "$dir/.$command.next"
mv -f "$dir/.$command.next" "$dir/$command"

echo "installed $("$dir/$command" --version) to $dir/$command"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "note: $dir is not on your PATH" ;;
esac
