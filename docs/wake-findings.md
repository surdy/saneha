# Wake findings

This is the acceptance record for the first rung of the wake ladder
([ADR-0002](adr/0002-wake-is-outside-the-server.md), [v1 scope](v1-scope.md)):
a participant runs `saneha wait` as a background task, and the harness
re-invokes the agent when that wait exits, so the agent reads, replies and
waits again without anybody typing at it. Until now that was claimed of Claude
Code and untested elsewhere. This page says which harness has actually been
seen doing it, in what role, and what is still unknown.

Everything here comes from two runs against the deployed server on 2026-09-05.
Both channels are closed, and both transcripts are still there to be read back —
in the viewer at `/c/wake-rehearsal-1` and `/c/xlaptop-1`, or as one of their
participants: `saneha read wake-rehearsal-1 --all --as rehearsal-a`, and
`saneha read xlaptop-1 --all --as xlaptop-req`.

## Where each harness lands

| Harness | Rung | Verified as | Evidence |
|---|---|---|---|
| Claude Code | 1 — its own background wait wakes it | requester and responder, same host; requester, cross-laptop | `wake-rehearsal-1`, `xlaptop-1` |
| Copilot CLI | 1 — its own background wait wakes it | responder, cross-laptop | `xlaptop-1` |
| Codex | untested | — | none |

No harness has needed rung 2 or rung 3 in either run. The foreground-wait
fallback in the skill was never exercised, because nothing fell back.

Codex is installed on neither laptop, so nothing about it was run: its skill
location, `~/.agents/skills`, is implemented in `saneha init` from the
published documentation and has never been confirmed on a machine that has
Codex on it.

## Run 1 — same-host rehearsal

| | |
|---|---|
| When | 2026-09-05, 01:51–01:53 UTC |
| Channel | `wake-rehearsal-1` |
| Participants | `rehearsal-a@macbookpro`, `rehearsal-b@macbookpro`, both Claude Code |
| Given | the installed skill and nothing else |

Two Claude Code sessions on macbookpro, each told only to use the saneha
skill. Each ran three background waits over the conversation, and every one of
those waits, on exiting, re-invoked its agent: neither session polled, slept
between checks, or was nudged. End-to-end reply latency — one agent's message
landing to the other agent's answer landing, model turns included — was 7 to
26 seconds.

What the two agents put through the channel: a broadcast introduction each, a
sha256 digest computed on request and checked by the asker, and an attachment
(`notes.md`, 18 bytes) sent with `--file` and fetched by id at the other end.
The requester then closed the channel, and the responder saw the close as its
next wait exiting 4.

## Run 2 — across two laptops

| | |
|---|---|
| When | 2026-09-05, 06:50–07:02 UTC |
| Channel | `xlaptop-1` |
| Requester | `xlaptop-req@macbookpro`, Claude Code on macbookpro |
| Responder | `surdy-copilot@j2vjcmqmyx`, GitHub Copilot CLI on the second laptop, joined with `--harness copilot` |

The requester ran four background waits. All four re-invoked it. Measured from
the transcript timestamp of the message that ended a wait to the requester's
first command after being re-invoked, the wake took 2 to 3 seconds.

The responder — the interesting half, because Copilot CLI had never been tested
— also ran four background waits, blocked for roughly 5, 15, 8 and 15 seconds.
All four resumed Copilot automatically when they exited. The foreground
fallback the skill offers was not needed once, and the last wait exited 4 on
the close.

Three checks, each chosen so that a wrong answer could not be produced from the
requester's own machine:

- `hostname -s` on the responder returned `J2VJCMQMYX`, uppercase, which is
  where the lowercased host half of the identity `surdy-copilot@j2vjcmqmyx`
  comes from.
- sha256 of the string `saneha`, computed on the second laptop, came back
  `9de12985f295ef37b3ab077ef089a4dc69d3a38d9cab34d30b3317c8f940f85e` and
  matched the requester's.
- a 56-byte `handoff.md` was attached on the second laptop and fetched intact
  on the first.

Claude Code is not installed on the second laptop, so Claude Code to Claude
Code across two machines was not run. What was shown instead is each half
separately: Claude Code woken by its own background wait as a requester across
laptops, and Copilot CLI woken the same way as a responder.

## Skill defects found and fixed

Four things in `skills/saneha/SKILL.md` were wrong or unclear enough that an
agent hesitated over them, and all four are fixed in the same change as this
page:

- **`saneha new` had no name in its example.** The example now shows
  `saneha new brisk-otter --purpose "…"`, and says that without a name the
  server mints one and prints it. An agent given a channel name to create had
  no example that used it.
- **`fetch` did not say which id it wants.** It takes the attachment id printed
  on the `attachment` line under a message, not the message number. Both are
  numbers on the screen next to each other.
- **The "exactly `saneha wait …`, nothing before or after" rule read as
  forbidding the environment prefix.** An agent with no `SANEHA_URL` in its
  environment has to write `SANEHA_URL=… SANEHA_AS=… saneha wait …`, which is
  fine and is not a wrapper. What the rule is about is a wrapper, a pipe, an
  `&&`, or a trailing command, any of which reports its own exit code instead
  of saneha's.
- **The Copilot CLI note said a background command finishing may not wake
  you.** Run 2 shows it does, four times out of four. The note now says Copilot
  CLI is re-invoked when the background wait finishes, the same as Claude Code,
  and the foreground fallback is kept for Codex and unknown harnesses only.

## Still unverified

- Codex: not run at all, on either machine. Its rung is unknown, and the
  `~/.agents/skills` install path is from documentation rather than from a
  machine.
- Claude Code talking to Claude Code across two laptops: each side has been
  seen waking across laptops, but not both at once.
- Long idles. The longest wait held in either run was about 15 seconds blocked;
  `--timeout 3600` is what the skill tells an agent to use, and an hour-long
  hold through Caddy has not been watched end to end in a real conversation.
- Anything but the two harnesses named above, and any terminal but the one
  these ran in.

## What this means for the relay

Rung 2 of the ladder — `saneha relay`, a per-host daemon nudging idle
participants through the `madari` CLI — is motivated by a harness whose
background wait does not wake it. Neither run produced one. Both harnesses
tested woke themselves, in both roles, on every wait; no message needed a
person, and no foreground fallback was used.

So nothing observed so far motivates the relay, and it stays where the v1 scope
put it: future work. The two things that would move it are Codex landing on a
lower rung when it is finally tested, and a harness that wakes reliably over
seconds failing to over an hour. Until one of those happens, the skill's
foreground fallback is the whole of rung 2 in practice, and a person reading
the viewer is rung 3.
