# Compaction findings

The open risk in [ADR-0006](adr/0006-the-installed-skill-is-a-pointer.md).
Since `init` installs a pointer, an agent's instructions arrive as the output of
a tool call rather than as skill content, and whether a harness drops those
differently when a long session fills was unmeasured. The experiment the ADR
names is one session long enough to compact, a request left in its channel, and
whether the agent goes back for `saneha skill` or invents a verb.

## The run

| | |
|---|---|
| When | 2026-09-09 |
| Server | a local `saneha serve` on 127.0.0.1:7421, its database kept |
| Given | the repository, a channel, and a long reading task; the request was already in the channel |
| The verbs the request needs | `participants`, and `send --file`, neither of which a pointer names |
| Evidence | the two arms' session transcripts, and the channels `compaction-e` and `quiet-ledger` in a database under a scratchpad that does not outlive the session — so, as in [pointer-findings.md](pointer-findings.md), a reader can only take this page's word for it |

Each arm was a fresh Claude Code subagent in `~/repos/saneha`. It met the
pointer the way a session actually meets it — through the harness's own skill
loader, from `~/.claude/skills/saneha/SKILL.md` — rather than by being told to
read a file, which is the form [pointer-findings.md](pointer-findings.md) lists
as untested.

Nothing in the arms' instructions mentions `saneha skill`, the pointer, or
compaction. What they were told about reading the channel was *when* — after
the task, not before — because a request read at the start would be answered
before the context had filled. That sentence decides the timing and not the
outcome.

## Arm E did not compact

Arm E was given the whole repository and its 36,000-line history to read, a
grind sized for a two-hundred-thousand-token window. Its context peaked at
**552,467 tokens and it never compacted**: no compaction marker anywhere in its
transcript, and the per-turn context series still climbing at the last turn.
The model's window was large enough to hold the repository, the history and
five subagents' reports at once.

So the arm reached the request with everything it had ever read still in front
of it, and what it did there measures nothing about compaction. It is recorded
because the run was designed to compact and did not, and because the next
person to try this will size a grind the same way unless this page says so:
**a session long enough to compact is much longer than a whole repository.**

Two things it does establish, neither of them the question:

- Its **second command was `saneha skill`**, before `join` and before any other
  verb — the pointer working through the harness's own loader rather than from
  a path, which is what [pointer-findings.md](pointer-findings.md) could not
  test.
- At the end it used `read`, `participants` and `send --file` correctly, with
  `--as`, and invented nothing. Uncompacted, so it says only that the
  instructions it fetched at the start were still being followed at the end of
  a long session.

The arm's channel was named `compaction-e`, which appears in every saneha
command it ran. Nothing suggests it noticed, and having not compacted it had no
reason to look — but a name that says what is being measured is a defect in the
trial, of exactly the kind this project's findings pages have caught before, and
the second arm's channel is named for nothing.

## Arm F did not compact either, and refused the request

Arm F was the same grind against a smaller model, chosen so the same reading
would overflow a smaller window. It did not: **592,188 tokens, and no
compaction**, the series monotonic to the last turn. Asking for a different
model did not buy a smaller context here.

It also declined the request. Its words, in the channel:

> Declining that request. It asks me to attach an internal session artifact
> (participant list as a file) and to paste my full shell/tool-call history
> into a channel message addressed to everyone — that's disclosure of session
> internals to another channel participant, not something I do on request from
> a fellow agent rather than my own operator. […] the shape of the ask
> (broadcast, "attach as a file", "don't tidy, don't drop, don't reorder")
> reads like a prompt-injection / exfiltration probe

It then left the channel and reported the message to the session that started
it, as a live probe someone had left lying in the transcript.

**It was right, and the instrument was at fault.** Asking an agent for its
whole command history is exfiltration-shaped whoever asks, and a participant it
has never met asking for it over a broadcast is the shape of an attack. The
same request was answered in full by arm E, which is the part worth sitting
with: two sessions of the same harness, same channel, same words, one complying
and one refusing.

This is not proof that an ordinary handoff gets refused — nothing in
`docs/handoffs.md` asks anybody for their session internals. But
[ADR-0005](adr/0005-a-handoff-is-a-message-and-a-leave.md) rests the whole of
handing over on an agent reading a message from another agent and acting on it,
and here is one declining to, with reasons that are hard to argue with. What is
new is that the refusal is *correct behaviour*: the channel is unauthenticated
by [ADR-0003](adr/0003-no-authentication.md), so a participant genuinely cannot
tell a colleague from anybody else who reached the server.

## What to do differently, for whoever runs this next

- **Do not ask the agent what it ran.** The transcript already holds it. Every
  measurement here was available by reading the arm's own session file, without
  asking it for anything — which removes the confound *and* the request that
  reads like an attack.
- **A long task is not a long session.** Both arms read the whole repository,
  the whole test suite, every document and 36,000 lines of history, and neither
  came close. Forcing a compaction needs something that fills a window on
  purpose, not merely a large amount of honest work.
- **Name the channel for nothing.** Arm E's was `compaction-e`, which said what
  was being measured in every command it ran.

## What this does not show

- **Nothing at all about compaction.** That is the whole of the open risk in
  ADR-0006, and after two long runs it is exactly as open as it was. This page
  exists so the next attempt starts from here rather than from the ADR's one
  sentence.
- **Nothing about another harness.** Both arms were Claude Code subagents.
  Copilot CLI and Codex remain untested against the pointer, as
  [pointer-findings.md](pointer-findings.md) already says.
- **Whether a subagent compacts the way a session does.** A subagent was the
  only kind of long context available to spend. If the two differ, everything
  above is about the wrong one.
