#!/usr/bin/env bash
# Screenshots of Phone and Messages with everything running, in a
# headless Weston inside a Docker container (never on your own desktop):
# the agent (omatel-agent) on private "system" and session buses, a
# scripted ModemManager (omatel-mock-mm) and fictional demo data. A call
# rings, is answered, muted and hung up through the agent's D-Bus API; a
# message arrives and one is sent. Also records the agent's and the apps'
# memory and the time to the first frame (on this machine, not a phone).
# The apps come from their own checkouts: OMAPHONE (default ../omaphone)
# and OMAMESSAGES (default ../omamessages), built here in release mode.
# Writes target/ui-shots/live/. Needs Docker and the omacam-uitest:2 image.
set -euo pipefail
cd "$(dirname "$0")/.."
IMAGE=${UI_IMAGE:-omacam-uitest:2}
phone_dir=${OMAPHONE:-$PWD/../omaphone}
messages_dir=${OMAMESSAGES:-$PWD/../omamessages}
for d in "$phone_dir" "$messages_dir"; do
  [[ -f $d/Cargo.toml ]] || { echo "no app checkout at $d (set OMAPHONE / OMAMESSAGES)" >&2; exit 1; }
done
cargo build --release -p omatel-agent -p omatel-mock \
  --bin omatel-agent --bin omatel-demo-data --bin omatel-mock-mm
(cd "$phone_dir" && cargo build --release --locked)
(cd "$messages_dir" && cargo build --release --locked)
out=target/ui-shots/live
rm -rf "$out"; mkdir -p "$out"; chmod 777 "$out"
size=${SIZE:-456x900}
passwd=$(mktemp); trap 'rm -f "$passwd"' EXIT
printf 'root:x:0:0::/root:/bin/bash\nuser:x:%s:%s::/tmp/home:/bin/bash\n' "$(id -u)" "$(id -g)" >"$passwd"
chmod 644 "$passwd"
mounts=()
for b in omatel-agent omatel-demo-data omatel-mock-mm; do
  mounts+=(-v "$PWD/target/release/$b:/usr/local/bin/$b:ro")
done
mounts+=(-v "$phone_dir/target/release/omaphone:/usr/local/bin/omaphone:ro")
mounts+=(-v "$messages_dir/target/release/omamessages:/usr/local/bin/omamessages:ro")
docker run --rm --user "$(id -u):$(id -g)" -e W="${size%x*}" -e H="${size#*x}" \
  "${mounts[@]}" -v "$passwd:/etc/passwd:ro" -v "$PWD/$out:/out" "$IMAGE" bash -c '
  set -u
  export XDG_RUNTIME_DIR=/tmp/rt HOME=/tmp/home XDG_DATA_HOME=/tmp/data XDG_CONFIG_HOME=/tmp/config
  export LANG=C.UTF-8 OMATEL_REGION=US TZ=UTC
  mkdir -m 700 -p $XDG_RUNTIME_DIR $HOME
  omatel-demo-data >/dev/null
  dbus-daemon --session --address=unix:path=/tmp/rt/system --fork --nopidfile
  dbus-daemon --session --address=unix:path=/tmp/rt/bus --fork --nopidfile
  export DBUS_SYSTEM_BUS_ADDRESS=unix:path=/tmp/rt/system DBUS_SESSION_BUS_ADDRESS=unix:path=/tmp/rt/bus
  sysbus() { dbus-send --bus=$DBUS_SYSTEM_BUS_ADDRESS --print-reply=literal --dest=org.freedesktop.ModemManager1 /org/omatel/MockModem "$@"; }
  phone() { dbus-send --session --print-reply=literal --dest=org.omamobile.Phone1 /org/omamobile/Phone1 "$@"; }
  messages() { dbus-send --session --print-reply=literal --dest=org.omamobile.Messages1 /org/omamobile/Messages1 "$@"; }
  rss() { awk "/VmRSS/ {print \$2/1024 \" MB\"}" /proc/$1/status; }
  omatel-mock-mm 2>/dev/null &
  sleep 0.5
  ms() { echo $(( ($(date +%s%N) - $1) / 1000000 )); }
  start=$(date +%s%N)
  omatel-agent --no-launch 2>/out/agent.log &
  agent=$!
  until dbus-send --session --print-reply --dest=org.freedesktop.DBus / org.freedesktop.DBus.NameHasOwner string:org.omamobile.Phone1 2>/dev/null | grep -q true; do sleep 0.02; done
  echo "omatel-agent: on the bus $(ms $start) ms after start" >/out/measurements.txt
  weston --backend=headless --shell=kiosk --width=$W --height=$H \
    --socket=wayland-9 --idle-time=0 >/tmp/weston.log 2>&1 &
  sleep 2
  export WAYLAND_DISPLAY=wayland-9 GSK_RENDERER=cairo

  # Time to the first frame: launch and save one shot, then quit. The
  # first round warms the font and icon caches; the second is recorded.
  for app in omaphone omamessages; do
    timeout 30 $app --script "shot /tmp/warm-$app.png; quit" >/dev/null 2>&1
  done
  for app in omaphone omamessages; do
    s=$(date +%s%N); timeout 30 $app --script "shot /tmp/first-$app.png; quit" >/dev/null 2>&1
    echo "$app: $(ms $s) ms from start to the first frame saved and exit" >>/out/measurements.txt
  done

  # Phone: a call rings, is answered, muted, and the far end hangs up.
  (
    sleep 3
    call=$(sysbus org.omatel.MockModem.Ring string:+12025550101 | tr -d " ")
    echo "$call" >/tmp/call
    sleep 2.5; phone org.omamobile.Phone1.Answer string:$call >/dev/null
    sleep 1.5; phone org.omamobile.Phone1.SetMute boolean:true >/dev/null
    sleep 4; sysbus org.omatel.MockModem.RemoteHangup objpath:$call >/dev/null
  ) &
  driver=$!
  timeout 60 omaphone --script "
    wait 2; shot /out/phone-keypad.png;
    wait 2; shot /out/phone-incoming.png;
    wait 1.5; shot /out/phone-in-call.png;
    wait 2; shot /out/phone-muted.png;
    wait 3; shot /out/phone-call-ended.png;
    wait 2; tab recents; wait 0.8; shot /out/phone-recents.png;
    wait 0.5; quit" 2>&1 | grep -E "WARN|ERROR|panicked" &
  app=$!
  sleep 6; pid=$(pgrep -x omaphone || true)
  [ -n "$pid" ] && echo "omaphone: $(rss $pid) RSS during a call" >>/out/measurements.txt
  wait $driver $app

  # Messages: a text arrives, the thread opens, a reply goes out.
  (
    sleep 3; sysbus org.omatel.MockModem.ReceiveSms string:+12025550103 string:"Are you around later?" >/dev/null
    sleep 4; messages org.omamobile.Messages1.Send string:+12025550103 string:"Yes, after six" >/dev/null
  ) &
  driver=$!
  timeout 60 omamessages --script "
    wait 2; shot /out/messages-list.png;
    wait 2.5; shot /out/messages-arrived.png;
    open 1; wait 3; shot /out/messages-thread-reply.png;
    wait 0.5; quit" 2>&1 | grep -E "WARN|ERROR|panicked" &
  app=$!
  sleep 5; pid=$(pgrep -x omamessages || true)
  [ -n "$pid" ] && echo "omamessages: $(rss $pid) RSS with a thread open" >>/out/measurements.txt
  wait $driver $app
  echo "omatel-agent: $(rss $agent) RSS after a call and two messages" >>/out/measurements.txt
  kill $agent
  exit 0
' || true
ls "$out"; cat "$out/measurements.txt" 2>/dev/null
if grep -E "2025550|555-01|around later|after six" "$out/agent.log"; then
  echo "the agent log holds a number or text" >&2; exit 1
fi
