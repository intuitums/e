#!/bin/sh
# One repository entry point. Keep CI, contributor docs, and local checks on
# the same commands so no environment has a private definition of "green".
set -eu
cd "$(dirname "$0")"

usage() {
  echo "usage: ./x [dev|scenario|preview|hooks|check|test|ui|fmt|lint|guard|bench|release-check] [args...]" >&2
  exit 2
}

command=${1:-check}
if [ "$#" -gt 0 ]; then
  shift
fi

case "$command" in
  dev)
    unset E_BUILD_VERSION E_BUILD_CHANNEL E_BUILD_COMMIT
    project=${1:-$PWD}
    if [ "$#" -gt 0 ]; then shift; fi
    project=$(CDPATH= cd "$project" && pwd)
    cargo build --locked
    binary="$PWD/target/debug/e"
    cd "$project"
    exec "$binary" "$@"
    ;;
  scenario)
    cargo build --locked
    exec python3 scripts/scenario.py "$@"
    ;;
  preview)
    exec python3 scripts/release/preview.py "$@"
    ;;
  hooks)
    [ "$#" -eq 0 ] || usage
    exec python3 scripts/hooks/install.py
    ;;
  check)
    [ "$#" -eq 0 ] || usage
    cargo fmt --check
    cargo fmt --manifest-path fuzz/Cargo.toml --check
    cargo clippy --all-targets -- -D warnings
    cargo test --locked
    # The published crates: the application is packaged and built end to end,
    # and the SDK's file list is checked (it cannot resolve its own dependency
    # until the application is on the registry, which the release publishes
    # first).
    cargo publish --dry-run --locked --allow-dirty -p intuitums-e
    cargo package --list --allow-dirty -p intuitums-e-sdk
    ./scripts/guard.sh
    python3 -m unittest discover -s scripts/release -p 'test_*.py'
    python3 -m unittest discover -s scripts/hooks -p 'test_*.py'
    ;;
  test)
    cargo test --locked "$@"
    ;;
  ui)
    cargo build --locked
    # First run creates the env under target/ (gitignored, gone with
    # `cargo clean`); PYTHON points at another interpreter instead.
    if [ -z "${PYTHON:-}" ]; then
      PYTHON=target/ui-env/bin/python
      # The marker records that requirements installed successfully; without
      # it an interrupted or failed pip leaves a reusable-looking venv whose
      # interpreter cannot import pyte, and every later run would skip the
      # repair instead of installing again.
      if [ ! -x "$PYTHON" ] || [ ! -f target/ui-env/.requirements-installed ]; then
        python3 -m venv target/ui-env
        target/ui-env/bin/pip install --quiet -r tests/ui/requirements.txt
        : > target/ui-env/.requirements-installed
      fi
    fi
    "$PYTHON" tests/ui/run.py "$@"
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
