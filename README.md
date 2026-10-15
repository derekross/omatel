# omatel

omatel is the shared telephony core of Omarchy Mobile, and the background
service behind its **Phone** and **Messages** apps. The apps live in their
own repositories:

- [omaphone](https://github.com/derekross/omaphone): Phone. Dialpad with
  smart dial, call log, favorites, contacts, and the in-call and answer
  screens.
- [omamessages](https://github.com/derekross/omamessages): Messages. SMS
  conversations with search, drafts, delivery reports and blocking.

This repository holds what both apps share. The crates are Rust, and the
apps depend on them as git dependencies pinned to a release tag:

| Crate | What |
| --- | --- |
| `omatel-mm` | zbus proxies for ModemManager 1.24's API: Modem, Voice, Call, Messaging, Sms, Sim, Signal |
| `omatel-store` | the SQLite stores: messages with full-text search, the call log, one shared block list |
| `omatel-contacts` | the address book, a vCard folder (vdir); number matching and smart dial |
| `omatel-notify` | desktop notifications with buttons and inline reply |
| `omatel-theme` | the Omarchy theme for GTK 4 and libadwaita, and the apps' shared stylesheet |
| `omatel-api` | the agent's session-bus API (`org.omamobile.Phone1`, `org.omamobile.Messages1`) and the shared settings |
| `omatel-mock` | test doubles: a scripted ModemManager, a notification server, fictional demo data (never shipped) |

**omatel-agent** runs from login, whether or not an app is open:
- It talks to the modem through ModemManager's D-Bus API. On Omarchy Mobile
  phones that API comes from `omamobile-mmd`, a bridge to the phone's own
  modem stack; elsewhere it comes from ModemManager itself.
- It rings for incoming calls (through feedbackd), shows a notification
  with Answer and Decline, and opens Phone's answer screen.
- It hangs up on blocked numbers, writes the call log, and notifies you of
  missed calls.
- It stores every incoming SMS before it removes it from the modem, shows
  one notification per conversation with an inline reply, sends messages
  and records delivery reports. While the phone is locked, notifications
  say only "New message" unless you allow more in Messages' settings.

> **Emergency calls (911, 112) don't work on this phone yet. Keep another
> phone for emergencies.** The agent refuses emergency numbers, and Phone
> shows a full-screen message instead of placing the call.

Not yet:
- MMS and group messages (they arrive with mmsd-tng).
- Call audio routing: speaker and mute go to callaudiod once Omarchy
  Mobile's call audio work lands.
- Hold, swap and conference calls; visual voicemail.
- CardDAV sync: the address book is a plain vCard folder that a sync tool
  can keep in step with a server.
- Emergency calls.

## Install

On Omarchy Mobile, install `omaphone` and `omamessages`; both bring in the
`omatel-agent` package. Then start the agent once:

```
systemctl --user enable --now omatel-agent
```

D-Bus activation also starts it when an app needs it. To build the package
from a checkout, run `bin/install` on the phone, or `bin/cross package` on
an x86_64 laptop (see [docs/development.md](docs/development.md)).

## Where things are kept

| What | Where |
| --- | --- |
| Messages, call log, block list | `~/.local/share/omatel/{messages,calls,blocked}.db` (SQLite, mode 0600) |
| Contacts | `~/.local/share/contacts/<book>/*.vcf`, one file per contact. New contacts go into `default` |
| Settings | `~/.config/omatel/settings.toml` |
| Logs | `journalctl -t omatel-agent`, `-t omaphone`, `-t omamessages` (states only, never numbers or text) |

## More

- [docs/architecture.md](docs/architecture.md): how the agent, the apps and
  the modem fit together, and why.
- [docs/development.md](docs/development.md): building, cross-compiling,
  and testing without a modem.
- [docs/export-format.md](docs/export-format.md): the JSON export of
  messages and the call log.

omatel is released under the GNU General Public License, version 3 or
later. Its theme support is adapted from
[gliff](https://github.com/omacom/gliff) (MIT); see NOTICE.
