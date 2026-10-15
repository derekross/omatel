#!/usr/bin/env bash
# Screenshots of Phone at the size it gets on the Pixel 7 Pro (456x900
# logical), in a headless Weston's kiosk shell inside a Docker container:
# never on your own desktop. Fictional demo data (omatel-demo-data) is
# written inside the container; no agent runs there, so the call pages
# are driven by the script's fake-incoming and fake-call steps.
# Writes target/ui-shots/omaphone/*.png. SIZE=WxH changes the size.
set -euo pipefail
cd "$(dirname "$0")/../.."
IMAGE=${UI_IMAGE:-omacam-uitest:2}
cargo build --release --locked -p omaphone -p omatel-mock --bin omaphone --bin omatel-demo-data
out=target/ui-shots/omaphone
rm -rf "$out"; mkdir -p "$out"; chmod 777 "$out"
size=${SIZE:-456x900}
docker run --rm --user "$(id -u):$(id -g)" -e W="${size%x*}" -e H="${size#*x}" \
  -v "$PWD/target/release/omaphone:/omaphone:ro" \
  -v "$PWD/target/release/omatel-demo-data:/omatel-demo-data:ro" \
  -v "$PWD/$out:/out" "$IMAGE" bash -c '
  export XDG_RUNTIME_DIR=/tmp/rt HOME=/tmp/home XDG_DATA_HOME=/tmp/data XDG_CONFIG_HOME=/tmp/config
  export LANG=en_US.UTF-8 TZ=UTC OMATEL_REGION=US
  mkdir -m 700 -p $XDG_RUNTIME_DIR $HOME
  /omatel-demo-data >/dev/null
  # No session bus and no agent: the app shows its "service is not running" banner.
  weston --backend=headless --shell=kiosk --width=$W --height=$H \
    --socket=wayland-9 --idle-time=0 >/tmp/weston.log 2>&1 &
  sleep 2
  WAYLAND_DISPLAY=wayland-9 GSK_RENDERER=cairo timeout 90 /omaphone --script "
    wait 1.5; tab keypad; shot /out/keypad.png;
    type 55501; wait 0.5; shot /out/keypad-typed.png;
    tab favorites; wait 0.5; shot /out/favorites.png;
    tab recents; wait 0.5; shot /out/recents.png;
    details 1; wait 0.6; shot /out/call-details.png; back; wait 0.4;
    tab contacts; wait 0.5; shot /out/contacts.png;
    open-contact 1; wait 0.6; shot /out/contact.png;
    edit-contact; wait 0.6; shot /out/contact-edit.png; back; back; wait 0.4;
    new-contact; wait 0.6; shot /out/contact-new.png; back; wait 0.4;
    fake-incoming +12025550101; wait 0.6; shot /out/incoming.png;
    fake-call +12025550100 active; wait 1.2; shot /out/in-call.png;
    fake-call +12025550100 ended; wait 2; 
    emergency; wait 0.5; shot /out/emergency.png;
    quit" 2>&1 | grep -E "WARN|ERROR|window saved|panicked" || true
'
ls "$out"
