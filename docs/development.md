# Developing omatel

How to build omatel, cross-compile it for a phone, and test it without a
modem. The design is in `docs/architecture.md`.

## Build

```
bin/build        # cargo build --release -p omatel-agent
bin/install      # build and install the omatel-agent package (pkgbuild/PKGBUILD)
```

You need Rust, pkgconf, gtk4 (4.14 or later), libadwaita (1.5 or later)
and sqlite with their headers. The tests also need `dbus-daemon`, from the
`dbus` package.

## Cross-compile for the phone

`bin/cross` is omacam's cross-compiler setup. It uses the same Docker
image (`omacam-cross:1`: x86_64 Rust, the aarch64 standard library, clang
and lld) and omacam's aarch64 sysroot when there is one, since that
sysroot already has GTK, libadwaita, SQLite and D-Bus.

```
bin/cross sysroot     # only without omacam's sysroot (~/.cache/omatel/sysroot-aarch64)
bin/cross build       # target/aarch64-unknown-linux-gnu/release/omatel-agent
bin/cross package     # pkgbuild/omatel-agent-<version>-1-aarch64.pkg.tar.zst
```

## Tests

- **`scripts/check.sh`** runs rustfmt, clippy with `-D warnings`, and every
  test. Run it before each commit.
- **Unit tests** cover the logic in the crates and the apps' pure
  functions:
  - vCard parsing and writing, number matching, T9, the emergency list;
  - the stores, their migrations and export;
  - the theme;
  - the apps' URI parsers, SMS part counting, time formats and script
    parsers.
- **`crates/omatel-mm/tests`** runs the proxies against the scripted
  ModemManager (`omatel_mock::mm`) on a private dbus-daemon: an incoming
  call answered with touch tones, an outgoing call (answered, and
  refused), SMS in and out, a delivery report, a failed send, and a WAP
  push.
- **`crates/omatel-notify/tests`** runs the notification client against
  the recording server: replace, reply, action, close.
- **`apps/omatel-agent/tests/agent.rs`** runs the agent end to end on two
  private buses, a "system" bus with the mock ModemManager and a session
  bus with the mock notification server. It is driven through its own
  API, as the apps and the shell drive it. It covers:
  - calls: answered, missed, declined, blocked, outgoing with post-dial
    tones, busy, and emergency numbers refused;
  - messages: SMS stored then deleted, duplicates, WAP push left alone,
    inline reply, delivery report, retry, "New message" on a locked
    phone, muted, blocked and disabled notifications;
  - a modem that appears late.
- **`scripts/chain-test.sh`** runs the agent over the real stack in a
  container: the agent, then `omamobile-mmd` and `omamobile-sitd` from an
  Omarchy Mobile checkout (`OMARCHY_MOBILE`, default
  `~/Projects/omarchy-mobile`), with `tools/sit-sim` as the modem. It also
  checks that the agent's log holds no number and no text.

### Screenshots

These all run in a headless Weston inside a container, at the size the
apps get on the Pixel 7 Pro (456x900). They never open a window on your
desktop.

- Each app's own repository has a `scripts/ui-shots.sh`: every page, on
  fictional demo data, without the agent.
- `scripts/ui-shots-live.sh`: both apps, built from their checkouts next
  to this one (`OMAPHONE`, `OMAMESSAGES`), with the agent and the mock
  ModemManager running. A call rings, is answered through the agent, is
  muted and is hung up by the far end; a message arrives and a reply goes
  out. It also writes `measurements.txt`: the agent's and the apps'
  memory, and the time from start to the first frame. These are measured
  on the build machine, not on a phone.

The apps take a `--script` for this. The steps are listed at the top of
each app's `script.rs`.

### Trying it by hand in a container

`omatel-mock-mm` serves the mock ModemManager on the bus that
`DBUS_SYSTEM_BUS_ADDRESS` names, so point that at a private bus, never at
a real one. You drive it with `dbus-send` on `org.omatel.MockModem`:
Ring, RemoteAnswer, RemoteHangup, ReceiveSms, Deliver and SetAutoAnswer.
`omatel-demo-data DIR` writes fictional contacts, calls and messages.
`scripts/ui-shots-live.sh` shows the whole setup.

## Logs

The agent logs to the journal as `omatel-agent`, and the apps as
`omaphone` and `omamessages`. From a terminal they log to stderr instead.
Set `RUST_LOG=omatel_agent=debug` for more. Logs carry states, counts and
object paths only.

## On a phone someone is using

- Phone and Messages are safe to open: without a modem they show their
  data, and the agent reports `no-modem`.
- Never test emergency numbers. The agent refuses them anyway.
- Calls and SMS on a real SIM transmit and cost money, so the human runs
  them, following the runbook in Omarchy Mobile's
  `docs/TELEPHONY-APPS-PLAN.md`.
