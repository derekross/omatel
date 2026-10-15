# omatel

omatel holds Omarchy Mobile's **Phone** (`omaphone`) and **Messages**
(`omamessages`) apps and **omatel-agent**, the background service they
share. The apps are written in Rust with GTK 4 and libadwaita. They are
laid out for a phone held upright (480 logical pixels wide) and follow your
Omarchy theme.

The apps talk to the modem through ModemManager's D-Bus API. On Omarchy
Mobile phones that API comes from `omamobile-mmd`, a bridge to the phone's
own modem stack; on any other machine it comes from ModemManager itself.
They behave like AOSP's Dialer and Messaging apps, though none of AOSP's
code is used.

> **Emergency calls (911, 112) don't work on this phone yet. Keep another
> phone for emergencies.** The Phone app says so on its dialpad. If you
> dial an emergency number, it shows a full-screen message and never
> places the call.

## What you get

**Phone**
- A large dialpad with smart dial: typing digits suggests contacts by the
  letters on the keys, or by their numbers. Long-press 0 for +.
  Characters after `,` in a number are sent as touch tones once the call
  is answered.
- Favorites (your starred contacts, plus the numbers you call most),
  Recents (the call log, with missed calls in red), and Contacts. All
  three are tabs.
- Contacts: search, a contact page, an editor (name, numbers with labels,
  emails, a note, favorite), importing and exporting `.vcf` files, and
  blocking.
- The in-call screen: a call timer, mute, speaker, a keypad for touch
  tones, and hang up. The answer screen offers Answer, Decline and Reply
  with a message.
- `tel:` and `callto:` links open the dialpad with the number filled in.
  A **Contacts** launcher entry opens the Contacts tab.

**Messages**
- The conversation list, with pinned conversations first, archived and
  muted conversations, unread counts, drafts, and full-text search.
- Threads with bubbles, day separators and the state of each message
  (Sending, Sent, Delivered, Not sent with a tap to retry). Older
  messages load as you scroll up. The composer counts SMS parts.
- Writing to a contact or to any number, blocking, and settings.
- Export and import of all messages as JSON (see
  [docs/export-format.md](docs/export-format.md)).
- `sms:`, `smsto:`, `mms:` and `mmsto:` links, with `?body=`.

**The agent (`omatel-agent`)** runs from login, whether or not an app is
open:
- It rings for incoming calls (through feedbackd), shows a notification
  with Answer and Decline, and opens Phone's answer screen.
- It hangs up on blocked numbers, writes the call log, and notifies you
  of missed calls, with Call back and Message buttons.
- It stores every incoming SMS before it removes the message from the
  modem.
- It shows one notification per conversation, with an inline reply where
  your notification server supports one. While the phone is locked, the
  notification says only "New message", unless you change that in
  Messages' settings.
- It sends messages and records delivery reports.

Not yet:
- MMS and group messages (they arrive with mmsd-tng).
- Call audio routing: speaker and mute go to callaudiod once Omarchy
  Mobile's call audio work lands; until then they change nothing.
- Hold, swap and conference calls; visual voicemail.
- CardDAV sync: the address book is a plain vCard folder that a sync tool
  can keep in step with a server.
- Emergency calls.

## Install

On Omarchy Mobile, install the `omaphone` and `omamessages` packages,
which bring in `omatel-agent`. Then start the agent once:

```
systemctl --user enable --now omatel-agent
```

To build the packages from a checkout instead, run `bin/install` on the
phone, or `bin/cross package` on an x86_64 laptop (see
[docs/development.md](docs/development.md)).

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

omatel is released under the GNU General Public License, version 3 or
later. Its theme support is adapted from
[gliff](https://github.com/omacom/gliff) (MIT); see NOTICE.
