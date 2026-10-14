# omatel

Omarchy Mobile's Phone (`omaphone`) and Messages (`omamessages`) apps and
the headless agent they share (`omatel-agent`), in one Rust workspace.

Run `scripts/check.sh` before every commit: rustfmt, clippy with warnings
as errors, and the tests. A clippy warning is a failure.

- The design is in `docs/architecture.md`; building, cross-compiling and
  testing in `docs/development.md`.
- **Privacy.** Never log a phone number, a name or message text: log
  states, counts and object paths only. Tests, fixtures and screenshots
  use fictional numbers (`+1 202 555 01xx`, the range reserved for
  fiction) and obviously made-up names only.
- **Never** open a window, a compositor or a test Hyprland on the
  developer's desktop: UI screenshots run in a headless Weston in a
  container (`scripts/ui-shots.sh`). D-Bus tests run on private
  dbus-daemons (`omatel_mock::bus`), never the machine's own buses.
- **Emergency calls are not supported yet.** The apps say so and the
  agent refuses to dial emergency numbers. Never place or test an
  emergency call.
- Logic without a UI lives in the crates (`crates/`), tested there. The
  apps talk to the modem only through the agent's session-bus API
  (`omatel-api`); only the agent talks to ModemManager (`omatel-mm`).
