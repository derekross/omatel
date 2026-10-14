# omatel's export format

Messages and the call log export to JSON files of their own. Each file
names its format and a version. An importer refuses another format and
any newer version. Files are written with mode 0600, because they hold
numbers and message text.

## Messages (`omamessages --export FILE`)

```json
{
  "format": "omatel-messages",
  "version": 1,
  "conversations": [
    {
      "address": "+12025550100",
      "numbers": ["(202) 555-0100"],
      "title": "",
      "pinned": true,
      "archived": false,
      "muted": false,
      "draft": "",
      "messages": [
        {
          "direction": "in",
          "transport": "sms",
          "sender": "",
          "body": "Dinner at seven?",
          "at": 1791970200000,
          "sent_at": 1791970195000,
          "delivered_at": null,
          "state": "received",
          "read": true
        }
      ]
    }
  ]
}
```

Fields:
- **`address`:** the conversation's key. It is the E.164 number when the
  number parses, otherwise the sender as given (a short code, or an
  alphanumeric sender name). A group's address is its numbers, sorted and
  joined with commas.
- **`numbers`:** the participants' numbers as first seen.
- **`at`:** when the message arrived on the phone, or when it was written
  (outgoing). It is in milliseconds since the epoch, as are `sent_at` and
  `delivered_at`.
- **`state`:** one of `received`, `sending`, `sent`, `delivered` or
  `failed`.
- **`transport`:** `sms` today, `mms` later.

Importing merges into existing conversations by `address`.
- A message whose direction, `at` and `body` all match one already stored
  is skipped, so importing the same file twice adds nothing.
- A message exported while still `sending` comes back as `failed`.

## Call log (`CallLog::export_json`)

```json
{
  "format": "omatel-calls",
  "version": 1,
  "calls": [
    {
      "number": "+12025550100",
      "raw_number": "(202) 555-0100",
      "direction": "in",
      "outcome": "answered",
      "started_at": 1791970200000,
      "answered_at": 1791970204000,
      "ended_at": 1791970516000,
      "duration": 312,
      "seen": true
    }
  ]
}
```

- **`outcome`:** one of `answered`, `missed`, `rejected`, `blocked`,
  `unanswered` or `failed`.
- **`duration`:** in seconds.
- On import, a call with the same number, direction and `started_at` as
  one already stored is skipped.

## Contacts

Contacts export and import as plain vCard (`.vcf`), from Phone's Contacts
menu. Export writes vCard 3.0. Import reads vCard 2.1 (including Android's
quoted-printable names), 3.0 and 4.0.
