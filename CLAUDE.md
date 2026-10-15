# omatel

The shared telephony core of Omarchy Mobile's Phone and Messages apps (the
crates in `crates/`) and `omatel-agent`, the background service both use.
The apps live in their own repositories, `omaphone` and `omamessages`, and
depend on these crates through git dependencies pinned to a release tag:
a change to a crate's API needs a new tag here and a dependency bump there.

Run `scripts/check.sh` before every commit: US spelling, rustfmt, clippy
with warnings as errors, and the tests. A clippy warning is a failure.

- The design is in `docs/architecture.md`; building, cross-compiling and
  testing in `docs/development.md`.
- **US English** in strings, identifiers, comments and docs
  (`scripts/spelling.sh`).
- **Privacy.** Never log a phone number, a name or message text: log
  states, counts and object paths only. Tests, fixtures and screenshots
  use fictional numbers (`+1 202 555 01xx`, the range reserved for
  fiction) and obviously made-up names only.
- **Never** open a window, a compositor or a test Hyprland on the
  developer's desktop: UI screenshots run in a headless Weston in a
  container (`scripts/ui-shots-live.sh`). D-Bus tests run on private
  dbus-daemons (`omatel_mock::bus`), never the machine's own buses.
- **Emergency calls are not supported yet.** The agent refuses to dial
  emergency numbers. Never place or test an emergency call.
- Only the agent talks to ModemManager (`omatel-mm`); the apps talk to the
  agent (`omatel-api`).
