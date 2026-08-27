#!/bin/sh
# Qualify the source tree (or a release tag) before artifacts are published.
set -eu
cd "$(dirname "$0")/.."

tag=${1:-}
identity=$(sed -n 's/^pub const VERSION: &str = "\([^"]*\)";/\1/p' src/lib.rs)
[ -n "$identity" ] || { echo "release-check: runtime version missing" >&2; exit 1; }

cargo metadata --locked --no-deps --format-version 1 >/dev/null

if [ -n "$tag" ]; then
  version=${tag#v}
  printf '%s\n' "$tag" | grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' || {
    echo "release-check: tag $tag is not an exact stable SemVer tag" >&2
    exit 1
  }
  [ "$identity" = "$version" ] || {
    echo "release-check: tag $tag does not match runtime version $identity" >&2
    exit 1
  }
  grep -Eq "^## $version( — [0-9]{4}-[0-9]{2}-[0-9]{2})?$" CHANGELOG.md || {
    echo "release-check: CHANGELOG.md has no section for $version" >&2
    exit 1
  }
fi

cargo build --release --locked
actual=$(./target/release/e --version)
[ "$actual" = "e $identity" ] || {
  echo "release-check: built binary says '$actual', expected 'e $identity'" >&2
  exit 1
}
echo "release-check: e $identity qualified"
