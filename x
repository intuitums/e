#!/bin/sh
# One repository entry point. Keep CI, contributor docs, and local checks on
# the same commands so no environment has a private definition of "green".
set -eu
cd "$(dirname "$0")"

usage() {
  echo "usage: ./x [check|test|fmt|lint|guard|bench|release-check] [args...]" >&2
  exit 2
}

command=${1:-check}
if [ "$#" -gt 0 ]; then
  shift
fi

case "$command" in
  check)
    [ "$#" -eq 0 ] || usage
    cargo fmt --check
    cargo fmt --manifest-path fuzz/Cargo.toml --check
    cargo clippy --all-targets -- -D warnings
    cargo test --locked
    ./scripts/guard.sh
    ;;
  test)
    cargo test --locked "$@"
    ;;
  fmt)
    cargo fmt "$@"
    cargo fmt --manifest-path fuzz/Cargo.toml "$@"
    ;;
  lint)
    cargo clippy --all-targets "$@" -- -D warnings
    ;;
  guard)
    [ "$#" -eq 0 ] || usage
    ./scripts/guard.sh
    ;;
  bench)
    [ "$#" -eq 0 ] || usage
    python3 benchmarks/run.py --build --check
    ;;
  release-check)
    ./scripts/release-check.sh "$@"
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage
    ;;
esac
