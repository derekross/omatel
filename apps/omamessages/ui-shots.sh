#!/usr/bin/env bash
# Screenshots of Messages on fictional demo data (omatel-demo-data), at the
# size the app gets on the Pixel 7 Pro (456x900 logical), in a headless
# Weston's kiosk shell inside a Docker container: never on your own
# desktop. A private session bus (dbus-run-session) and no agent, so the
# "service isn't running" banner shows. SIZE=WxH changes the size.
# Writes target/ui-shots/omamessages/*.png. Needs Docker and the
# omacam-uitest:2 image (omacam's scripts/ui-shots.sh builds it).
set -euo pipefail
cd "$(dirname "$0")/../.."
IMAGE=omacam-uitest:2
if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  docker build -t "$IMAGE" - <<'DOCKERFILE'
FROM archlinux:latest
RUN pacman -Syu --noconfirm --needed weston gtk4 libadwaita adwaita-icon-theme ttf-dejavu mesa && pacman -Scc --noconfirm
DOCKERFILE
fi
cargo build --release --locked -p omamessages -p omatel-mock --bin omamessages --bin omatel-demo-data
out=target/ui-shots/omamessages
rm -rf "$out"; mkdir -p "$out"; chmod 777 "$out"
size=${SIZE:-456x900}
# dbus-daemon needs our UID in the passwd database.
passwd=$(mktemp); trap 'rm -f "$passwd"' EXIT
printf 'root:x:0:0::/root:/bin/bash\nuser:x:%s:%s::/tmp/home:/bin/bash\n' "$(id -u)" "$(id -g)" >"$passwd"
chmod 644 "$passwd"
long=$(printf 'Running a little late, the bus is stuck behind a parade on Main Street. %.0s' 1 2 3)
docker run --rm --user "$(id -u):$(id -g)" -e W="${size%x*}" -e H="${size#*x}" -e LONG="$long" \
  -v "$PWD/target/release/omamessages:/omamessages:ro" \
  -v "$PWD/target/release/omatel-demo-data:/omatel-demo-data:ro" \
  -v "$passwd:/etc/passwd:ro" -v "$PWD/$out:/out" "$IMAGE" bash -c '
  export XDG_RUNTIME_DIR=/tmp/rt HOME=/tmp/home XDG_DATA_HOME=/tmp/data XDG_CONFIG_HOME=/tmp/config
  export LANG=C.UTF-8 OMATEL_REGION=US TZ=UTC
  mkdir -m 700 -p $XDG_RUNTIME_DIR $HOME
  /omatel-demo-data >/dev/null
  weston --backend=headless --shell=kiosk --width=$W --height=$H \
    --socket=wayland-9 --idle-time=0 >/tmp/weston.log 2>&1 &
  sleep 2
  WAYLAND_DISPLAY=wayland-9 GSK_RENDERER=cairo timeout 90 dbus-run-session -- /omamessages --script "
    wait 1.5; shot /out/01-list.png;
    open 0; wait 1; shot /out/02-thread-pinned.png; back; wait 0.5;
    open 4; wait 1; shot /out/03-thread-failed.png;
    type $LONG; wait 0.5; shot /out/04-composer-counter.png; type; back; wait 0.5;
    compose; wait 0.5; type sa; wait 0.5; shot /out/05-compose.png; back; wait 0.5;
    compose; type 202 555 0177; wait 0.5; shot /out/06-compose-number.png; back; wait 0.5;
    search dinner; wait 0.5; shot /out/07-search.png; search; wait 0.3;
    open-archived; wait 0.5; shot /out/08-archived.png; back; wait 0.5;
    menu 1; wait 0.8; shot /out/09-row-menu.png; close-menu; wait 0.5;
    settings; wait 0.8; shot /out/10-settings.png; close-settings; wait 0.5;
    blocked; wait 0.5; shot /out/11-blocked.png; back; wait 0.3; quit"
' 2>&1 | grep -E "WARN|ERROR|CRITICAL|window saved" || true
ls "$out"
