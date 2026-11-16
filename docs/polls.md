# Polls by convention

Rocket.Chat has no poll message type without an app. Rustket uses a plain-text
convention that every client can read and vote in, with reactions as votes.

## Format

```
📊 Where do we have lunch?
1️⃣ Pizza
2️⃣ Sushi
3️⃣ Salad
```

- The first line starts with `📊` (or `:bar_chart:`), a space and the question.
- Then 2 to 10 option lines, numbered from 1 in order, each starting with the
  keycap of its number and a space: `1️⃣` … `9️⃣`, `🔟` (with or without the
  U+FE0F variation selector), or the shortcodes `:one:` … `:nine:`,
  `:keycap_ten:` (`:ten:`).
- Blank lines and CRLF line ends are ignored. Anything else (a line without a
  keycap, a gap or a wrong order in the numbers, an empty option, a message
  with attachments) makes it an ordinary message.

Parsing and generation: `crates/rustket-core/src/poll.rs`.

## Votes

A vote is a reaction with the option's keycap. Rocket.Chat stores the keycaps
under these names (checked on 8.9.1):

| Option | Reaction |
|---|---|
| 1–9 | `:one:` `:two:` `:three:` `:four:` `:five:` `:six:` `:seven:` `:eight:` `:nine:` |
| 10 | `:keycap_ten:` |

Rustket draws a poll card instead of the text: each option with its vote
count, a bar relative to the leading option, a check on the user's own votes
and the voters in a tooltip. A click toggles the user's reaction
(`chat.react`). Nothing limits a user to one vote, as with reactions anywhere.

## Creating

The chart button in the composer opens "Create poll": the question, 2–10
options and "Add the option reactions". With that on, once the message is
sent Rustket adds every option's reaction as the sender (one `chat.react` per
option, waiting out rate limits), so other clients show one-click vote
buttons. The poll is sent like any message (encrypted in encrypted rooms).
Counts are the reactions as every client sees them, so with the option
reactions added the author counts once on every option.
