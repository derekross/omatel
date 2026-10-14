#!/usr/bin/env bash
# The agent over the real chain: omatel-agent -> omamobile-mmd ->
# omamobile-sitd -> tools/sit-sim, from an Omarchy Mobile checkout
# (OMARCHY_MOBILE, default ~/Projects/omarchy-mobile), in a container with
# Python, PyGObject and dbus (the bridge's conformance image), on private
# buses. Never the machine's own buses, never a phone.
set -euo pipefail
cd "$(dirname "$0")/.."
repo=${OMARCHY_MOBILE:-$HOME/Projects/omarchy-mobile}
[[ -x $repo/tools/sit-sim ]] || { echo "no Omarchy Mobile checkout at $repo" >&2; exit 1; }
image=omatel-chain:1
if ! docker image inspect "$image" >/dev/null 2>&1; then
  docker build -t "$image" - <<'DOCKERFILE'
FROM archlinux:latest
RUN pacman -Syu --noconfirm --needed python python-gobject dbus sqlite procps-ng && pacman -Scc --noconfirm
DOCKERFILE
fi
bin=$(cargo test -p omatel-agent --test chain --no-run --message-format=json 2>/dev/null |
  python3 -c 'import json,sys
for l in sys.stdin:
    m=json.loads(l)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="chain" and m.get("executable"): print(m["executable"])' | tail -1)
[[ -n $bin ]] || { echo "the chain test did not build" >&2; exit 1; }
# dbus-daemon's EXTERNAL authentication needs our user in /etc/passwd.
passwd=$(mktemp); trap 'rm -f "$passwd"' EXIT
{ echo "root:x:0:0:root:/root:/bin/bash"; echo "tester:x:$(id -u):$(id -g):tester:/tmp:/bin/bash"; } >"$passwd"
docker run --rm --user "$(id -u):$(id -g)" -e HOME=/tmp -e OMATEL_CHAIN_REPO=/repo \
  -v "$passwd:/etc/passwd:ro" -v "$repo:/repo:ro" -v "$bin:/chain:ro" "$image" \
  /chain --nocapture --test-threads=1
