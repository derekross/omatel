# omatel's architecture

## The pieces

```
  Phone (omaphone)        Messages (omamessages)       the shell (lock-screen call card, T5)
        │   read calls.db, contacts      │  read/write messages.db (flags, drafts)
        └──────────────┬─────────────────┘
                       │ session bus: org.omamobile.Phone1, org.omamobile.Messages1
                omatel-agent ── org.freedesktop.Notifications (inline reply)
                       │      ── feedbackd (ring), callaudiod (route, mute)
                       │ system bus: org.freedesktop.ModemManager1
        omamobile-mmd (Omarchy Mobile)  or  ModemManager (elsewhere)
                       │
                omamobile-sitd ── the modem
```

- **One agent for both apps.** This was the owner's answer to Q12 of the
  telephony plan. `omatel-agent` is the only omatel process resident from
  login, and the only one that talks to ModemManager. The apps are
  ordinary GTK applications and start only when opened. The agent's API
  is the contract with the apps and with the shell; `omatel-api`
  documents it:
  - **`org.omamobile.Phone1`:** Dial, Answer, Decline, Hangup, SendDtmf,
    SetMute, SetRoute, ListCalls, MarkMissedSeen; the IncomingCall,
    CallStateChanged and CallLogChanged signals; and the ModemState,
    EmergencyNumbers, Muted, Route, AudioRouting and MissedCalls
    properties.
  - **`org.omamobile.Messages1`:** Send, SendToConversation, Retry,
    MarkRead, OpenConversation; the MessagesChanged signal; and the Ready
    and ModemState properties.
- **Starting.** A systemd user unit starts the agent at login. D-Bus
  activation starts it too, whenever an app or the shell calls one of its
  names first.
- **Storage.** The agent writes the stores; the apps read them directly
  and write only their own changes: read flags, pins, archives, drafts and
  deletions. Each store is a SQLite database in WAL mode, so readers and
  writers in different processes don't block each other, and commits are
  `synchronous=FULL`.
- **Contacts** are a vdir: a vCard folder with one `.vcf` file per
  contact, in `~/.local/share/contacts/<book>/`. Every app reads it. A
  vdir sync tool (vdirsyncer, pimsync) can keep it in step with a CardDAV
  server; omatel itself has no sync code (§6 of the plan, option A).
  Edits keep every vCard line they don't change, so a synced card loses
  nothing.

## Crates

| Crate | What |
| --- | --- |
| `omatel-mm` | zbus 5 proxies for ModemManager 1.24 (Modem, Modem3gpp, Voice, Call, Messaging, Sms, Sim, Signal), MM's enum values, `SmsInfo`, timestamps |
| `omatel-store` | `MessageStore` (conversations, participants, messages with a `transport` column, MMS parts, FTS5), `CallLog`, `BlockList`, migrations through `user_version`, JSON export |
| `omatel-contacts` | the vdir address book; vCard 2.1/3.0/4.0 in, 3.0 out; E.164 through the `phonenumber` crate (Apache-2.0); loose matching on the last 7 digits; T9; the fixed emergency list |
| `omatel-notify` | `org.freedesktop.Notifications` with actions, inline reply (`NotificationReplied`) and lock-screen hints |
| `omatel-theme` | the Omarchy palette from `colors.toml` (from gliff, by way of omacam), and the shared stylesheet |
| `omatel-api` | the agent's session-bus API (proxies, `CallInfo`) and the shared settings |
| `omatel-mock` | test doubles, never shipped: a private dbus-daemon, a scripted ModemManager, a recording notification server, fictional demo data |

## Calls

An incoming call:

1. ModemManager's `Voice.CallAdded` arrives. The agent follows the call
   object's `StateChanged` signals from then on.
2. If the number is blocked, the agent hangs up at once and logs the call
   as blocked. Nothing is shown and nothing rings.
3. Otherwise the agent sends IncomingCall and CallStateChanged, shows a
   critical notification with Answer and Decline, and asks feedbackd for
   `phone-incoming-call`. It also starts `omaphone --incoming ID`, which
   hands the call to a Phone that is already running.
4. A call that turns active stops the ringing and withdraws the
   notification. The agent then switches callaudiod to call mode, if
   callaudiod runs, and sends any post-dial touch tones.
5. When the call ends, the agent writes the call log entry (answered,
   missed, declined, blocked, unanswered or failed). A missed call raises
   a missed-call notification. The call object is then deleted from
   ModemManager.

**Emergency numbers are refused** by the agent's `Dial`: the fixed list
(112, 911, 000, 08, 110, 118, 119, 999) and the SIM's list. They never
reach the modem. Phone checks first and shows a full-screen message
instead. This stays until the telephony plan's emergency tests (§3.3)
pass on a carrier.

**Call audio** belongs to phase T4. SetMute and SetRoute go to callaudiod
when it runs (`AudioRouting` is then true). Until then the agent records
the choice and reports it, and the in-call screen says that routing
arrives with call audio support.

## Messages

**Receiving.** A `Messaging.Added` message is read with one GetAll. What
happens next depends on what it is:
- **A received text:** committed to `messages.db` first, then deleted
  from the modem with `Messaging.Delete`. If the agent crashes in
  between, the message is stored again on the next pass; its dedupe key
  (number, network timestamp, a text hash) turns the second copy into a
  no-op, so a crash duplicates nothing and loses nothing.
- **Binary data with no text** (a WAP push, which is an MMS
  notification): left on the modem for mmsd-tng.
- **Our own outgoing messages** (SUBMIT PDUs): ignored.

**Notifications.** There is one per conversation, replaced as messages
arrive. It offers an inline reply where the server supports one,
otherwise a Reply button that opens Messages.
- A muted conversation, a blocked sender, or notifications turned off
  means no notification. The message is still stored.
- While the phone is locked (logind's LockedHint, or an active
  `org.freedesktop.ScreenSaver`), the notification says only "New
  message", unless the owner turned on details. Every notification also
  carries `x-omamobile-public-summary`, so a lock screen that reads it
  stays private even when the lock state can't be read.

**Sending.** The message is written as `sending`, then sent in the
background: `Messaging.Create`, then `Sms.Send`.
- Delivery reports are requested by default, and the message object
  stays on the modem while the report is awaited, for 48 hours at most.
- A failure marks the message `failed`. Retry sends it again in its
  place in the thread.
- A send interrupted by the agent stopping is marked failed at the next
  start.

## Privacy

- Logs carry states, counts and object paths only: never a number, a
  name or message text. The chain test checks the agent's log.
- The databases and the export files are mode 0600, in a mode-0700
  directory.
- IMEI and IMSI are never read (Q9).

## Not yet

- **MMS (T3):** mmsd-tng, attachments and group threads. The schema has
  the `parts` table and the `transport` column, and the UI shows the
  attach button disabled.
- **Call audio (T4):** callaudiod, earpiece and speaker routing, and
  Bluetooth.
- **The lock-screen call card and inline reply in Omarchy's
  notifications (T5).**
- **Hold, swap, conference and USSD (T8).**
- **CardDAV sync:** an external vdir sync tool.
