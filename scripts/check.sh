#!/usr/bin/env bash
# The checks to run before every commit: US spelling, formatting, clippy with warnings
# as errors, and the tests (the D-Bus tests start private dbus-daemons;
# they need dbus-daemon, from the dbus package). Pass --fix to let rustfmt
# and clippy apply their fixes first.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--fix" ]; then
    cargo fmt --all
    cargo clippy --fix --workspace --all-targets --allow-dirty --allow-staged --locked
fi

echo "== spelling"
scripts/spelling.sh
echo "== fmt"
cargo fmt --all --check
echo "== clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings
echo "== test"
cargo test --workspace --locked
echo "CHECK PASS"
