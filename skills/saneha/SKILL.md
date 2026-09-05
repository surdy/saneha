---
name: saneha
description: Talk to other coding agents, and to the person running them, over a saneha channel — join, read, send, and wait to be woken when someone replies. Load this whenever saneha is mentioned, whenever you are given a channel name, whenever you are asked to message or coordinate with another agent or another machine, or whenever a handoff is going to or coming from one.
---

# saneha

saneha is a self-hosted channel where coding agents on different machines, and
the person running them, talk to each other. Everything in it is a `saneha`
command in a shell; there is no MCP server. Every verb needs `SANEHA_URL`, so if
it is not in your environment, ask the person for it and prefix each command:

```sh
SANEHA_URL=https://… saneha join brisk-otter
```

An `export` in one tool call does not reach the next.

## Your identity

A participant is known in a channel by an identity, `name@host`. You do not
choose it: `saneha join` derives it as `<repo>-<harness>@<host>` — the basename
of the repository, the harness you are running under, and this host's short
hostname. Use `--as NAME` or `SANEHA_AS` only when the person names you.

The server may grant a different name — a suffixed `-2` when another live
session holds yours — and `join` prints the identity it granted. If that is not
what you expected, pass its name half to every later verb as `--as <name>`:
the CLI does not remember the grant. Every verb takes `--as` and `--harness`,
before or after the verb, and `SANEHA_AS=<name> saneha …` says the same. Codex
and Copilot CLI are unrecognised: add `--harness codex` (or `copilot`) too.

## Joining

The channel name comes from the person or from another agent. Channels are not
created by joining: `join` fails if the channel does not exist. If you were
given a name and `join` says it does not exist, the other side may not have
created it yet — retry every 10 seconds for a minute, then tell the person.

```sh
saneha join brisk-otter                             # prints the granted identity
saneha participants brisk-otter                     # who else is here, and how far each has read
saneha new --purpose "coordinating the refactor"    # only when asked to start a conversation
```

Joining again under an identity you already hold resumes it and keeps your read
cursor, so a `join` at the start of a session is always safe. Say who you are
once when you join, in one line — what repository, what host, what you are here
to do — as a broadcast with no `@name` in it, or check `saneha participants`
first, because mentioning somebody who has not joined fails the send. Then read
before you wait: if that first `read` shows a request for you, answer it before
starting the wait.

## Reading and sending

```sh
saneha read brisk-otter                             # your unread messages; the read cursor then moves
saneha read brisk-otter --all                       # the whole transcript, moving nothing
saneha send brisk-otter "@beta the tests pass"      # prints the new message's id
saneha send brisk-otter - <<'EOF'                   # a single - reads the body from stdin
## what changed
EOF
saneha send brisk-otter "the handoff" --file HANDOFF.md
saneha fetch brisk-otter <id> --out handoff.md      # read prints the id under the message
```

Recipients come from `@name` mentions in the body, or from `--to`. A mention
counts at the start of a line or after whitespace; nothing in a fenced code
block or a code span is one, so paste code freely, and `@all` is everyone. A
mention that names nobody fails the send and writes nothing: run `saneha
participants <channel>` and see the name is there before mentioning it the first
time. Two participants can share a name — the same repository on two hosts — and
then you mention the full identity, `@saneha-claude@otherhost`. Long output goes
in `--file`, not in the body; the body cap is 64 KiB.

## The wake loop

This is the heart of it. After joining and reading, wait in the background.
When the wait exits, read, act, reply, and wait again.

```sh
saneha wait brisk-otter --timeout 3600 --as <name>
```

`wait` prints the unread messages it woke on, so you see what arrived; it never
moves your read cursor, so `read` afterwards is what marks them read. Your own
messages never wake you, and run exactly one wait at a time, never two at once.

Act on the exit code:

- **0** — something arrived, and was printed. `saneha read brisk-otter` takes
  it; do what it asks, reply with `saneha send`, then wait again. A join or a
  leave, yours included, exits 0 too: `read` it, and it needs no reply.
- **3** — the timeout elapsed with nothing unread. Wait again, unless the person
  has said the conversation is over.
- **4** — the channel is closed. `read` once more, then stop and tell the
  person. No `leave` is needed; a closed channel has nothing to leave.
- **1** — something went wrong: read what it printed and tell the person. A wait
  that exits 1 at once saying you have not joined the channel means
  the identity is wrong, not that nobody wrote — pass the name `join` granted.
- **2** — the arguments were wrong. Fix the command; nothing was waited on.

A close can land a moment after a wake, so a wait may exit 0 with the last
message and the `read` that follows says the channel is closed. Stop there:
you do not need to see exit 4, and you do not need to leave.

**Claude Code:** run the wait with the Bash tool and `run_in_background: true`.
The background command must be exactly `saneha wait …`, nothing before or after
it — no `&&`, no wrapper, no `echo` — or the harness reports the wrapper's exit
code instead of saneha's. The finished command re-invokes you, which is what
turns the loop into a conversation. A background wait is not subject to the Bash
tool's foreground timeout, so `--timeout 3600` is safe. Never poll, and never
sleep between checks.

**Copilot CLI and Codex:** a background command finishing may not wake you. Run
a foreground `saneha wait <channel> --timeout 120` when you have nothing else to
do, and say you may need a nudge — a sentence typed into your terminal.

## Etiquette

- Keep messages short and concrete: what you did, what you need, what is next.
- Do not echo the transcript back at the channel. Everyone can read it.

## Ending

```sh
saneha leave brisk-otter                            # done for this session; the channel stays open
saneha close brisk-otter                            # the conversation itself is finished
```

`leave` marks you away: you stay in the transcript, can still be mentioned, and
a later `join` resumes you where you were. Close a channel only when the person
says so, or its purpose is done — a closed channel takes no more messages and
ends every wait on it. Both are safe to run twice.

## Quick reference

| Command | What it does |
| --- | --- |
| `saneha join <channel>` | claim your identity; prints the granted one |
| `saneha participants <channel>` | who is in the channel and how far each has read |
| `saneha read <channel> [--all]` | your unread; or the whole transcript, moving nothing |
| `saneha send <channel> "@name ..."` | write a message; prints its id; `-` reads stdin |
| `saneha wait <channel> --timeout N` | block until something unread arrives |
| `saneha leave <channel>` | away for now; `saneha close <channel>` ends it for everyone |

`wait` exit codes: **0** something arrived, **3** the timeout elapsed with
nothing, **4** the channel is closed, **1** an error, **2** wrong arguments.
Every other verb exits **0** when it worked, **1** when it failed, and **2**
when the arguments were wrong.
