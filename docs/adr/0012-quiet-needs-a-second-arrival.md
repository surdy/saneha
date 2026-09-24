---
status: accepted
date: 2026-09-24
---
# Quiet needs a second arrival

[ADR-0009](0009-quiet-is-a-view-of-the-participants.md) folds away the channels that are open, have a transcript, and have nobody present, and guards the fold with one case: a channel minted a moment ago and joined by nobody has no transcript, so it is new and stays in view. There is a second case the guard misses, and it is the one that matters most. A handoff is a message and a leave ([ADR-0005](0005-a-handoff-is-a-message-and-a-leave.md)): the session that is ending joins, writes what it knows, and leaves. The channel now has a transcript and nobody present, so the rail folds it — at the exact moment the person is carrying its name to a fresh session and looking for it. On the live server `steady-osprey` sat under that heading for a week, a handoff nobody had taken, indistinguishable in the rail from the fourteen that had been.

The first idea was to have the handing-over agent stay, so the channel would have somebody present until the receipt landed. It does not work, for three reasons that are worth writing down. The session that hands over is ending, so nobody is there to run its `leave` later; the exit has to be somebody else's act. The obvious somebody is the taker, and under [ADR-0010](0010-a-session-is-found-by-its-session.md) a fresh session on the same host under the same name resumes the hander's participant, so the taker's leave would be the hander's — but only then. If the old session is still open in another pane the taker is granted a suffixed name and leaves as itself, and on another host `--as` names a participant on *this* host, so nobody can leave for one elsewhere. In both, the hander stays present for good. And *present* would have stopped meaning present: a dead session shown as here is a lie told for a good reason, but the participants column, wake, and [ADR-0011](0011-the-server-may-be-asked-to-close-quiet-channels.md)'s sweep all read the word literally.

What the fold actually needs to know is whether anybody has arrived since the channel was written in, and the transcript already says. Every join writes a `join` system message, a resume included. So the server counts them, as `joins`, in the same set of columns `present` is counted in, and the rail's rule becomes: **a channel is quiet when it is open, nobody is present, and at least two joins have happened.** Two joins means a second arrival — the taker of a handoff, the second agent of a conversation, or the first one coming back — and the channel is a conversation that has run. One join means only the writer was ever there, and someone may still be expected, so the row stays under Open where the person waiting on it is looking. Zero joins is the minted channel, which the rule now covers without a separate guard.

Against the shapes that occur:

| shape | joins | present | rail |
|---|---|---|---|
| handoff posted, not yet taken | 1 | 0 | Open |
| handoff taken, same name resumed | 2 | 0 | Quiet |
| handoff taken, a different name | 2 | 0 | Quiet |
| two agents talked, both left | 2 or more | 0 | Quiet |
| minted, joined by nobody | 0 | 0 | Open |
| somebody parked on `wait` | any | 1 or more | Open |

The sweep reads quiet the same way. A channel the rail shows as waiting must not be closed as quiet behind the person's back, so `close_quiet_channels` carries the same two-join condition, and an untaken handoff is never closed by the clock. Whether it should be closed after a week is a question the person decides, from the row they can now see.

This is still ADR-0009's design. Nothing is stored, no verb or message kind is added, and no participant behaves differently: quiet is read off the transcript and the participants each time the rail is drawn, and one join is still enough to bring a row back. What changes is the definition, by one clause, so ADR-0009's vocabulary entry and README paragraph are amended rather than replaced.

## What is accepted

- **One shape is shown that is over.** An agent that joins alone, posts a note, and leaves is one join and nobody present, the same as an untaken handoff, and nothing in the transcript tells them apart. It stays under Open until somebody closes it. The rule errs toward showing a finished note rather than hiding a waiting handoff, and the row menu's `close` is one click.
- **The skill is unchanged.** Its sentence that a taken handoff "folds itself off the person's list" is still true, since the taker's join is the second one. It now also holds the converse without saying so: until then the channel is in view.

## Consequences

- **One field on the wire.** `joins` on every `Channel` the server describes, filled by a correlated subquery in `CHANNEL_COLUMNS` (`store.rs`), defaulted on deserialisation so a client meeting an older server reads 0 — which reads as not quiet, and so keeps a channel in view rather than hiding one. `saneha list` does not print it; `--json` carries it.
- **The rail's `quiet()` in `web/index.html` reads `joins >= 2`** in place of `newest_id > 0`. `tests/rail.js` carries `lone-heron`, an untaken handoff that must stay under Open, beside `still-heron`, the taken one that must fold.
- **The sweep's query carries the same clause**, and `tests/quiet.rs` holds `lone-otter`: old, a transcript, nobody present, one join, left alone.
