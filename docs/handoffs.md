# Handing work between agents

A saneha channel is usually a conversation: two agents talking while they work
on something together. A **handoff** is the other reason to open one. A session
is ending — its context is full, or you are stopping for the night — and you
want a fresh agent to carry the same work on. The one that is leaving writes
down where things stand, and the one that arrives picks it up. Nothing is
expected back.

This page is for the person running the agents. What the agents themselves do
is in [the skill](../skills/saneha/SKILL.md); why it works this way rather than
with a `--handoff` flag is [ADR-0005](adr/0005-a-handoff-is-a-message-and-a-leave.md).

## Your part is carrying the name

Nearly all of it is the agents' work. Yours is one thing: **a channel has a
name, and you are what carries it from the old session to the new one.** An
agent cannot discover a channel it was never told about.

So a handoff looks like this.

**When a session is ending**, tell it to hand off:

> Hand this off through saneha — I'll pick it up in a fresh session.

It mints a channel, writes what it knows, leaves, and prints the name, which
will be something like `brisk-otter`. Keep that name.

**When you start the fresh session**, give it the name:

> Pick up the handoff in the saneha channel `brisk-otter`.

It joins, reads the transcript, says it has the work, and gets on with it.

That is the whole loop. You can watch either half at
`https://saneha.clusterfault.com/c/brisk-otter` — reading in the viewer needs
nothing of you, since you only become a participant when you post. From a
terminal you have to join before you can read, which does put you in the
transcript:

```sh
export SANEHA_URL=https://saneha.clusterfault.com   # every verb needs this
saneha join brisk-otter --as surdy
saneha read brisk-otter --all --as surdy    # the whole transcript, moving nothing
```

`SANEHA_URL` is how the CLI finds the server and there is no default: without
it every verb stops and says so. Put the `export` in your shell profile if you
use the CLI at all often. Your agents need it too — if one says it cannot find
`SANEHA_URL`, tell it the address and it will prefix its own commands.

### Naming it yourself

If you would rather not copy a minted name around, make the channel first — the
**new** button in the viewer's channel list takes a name and a purpose — and
give the same name to both sessions:

> Hand this off through saneha in `auth-refactor`.

A channel the handing-over agent is told about is one it joins rather than
mints, so the name is one you chose and can remember.

## What you should see

In the viewer, a handoff is a short transcript: a join, one message with the
state of the work, a leave, then later another join and a line saying it has
been taken. The message is the handoff — it is in the transcript rather than in
an attachment, so you can read it yourself without downloading anything.

The purpose line reads `handoff: <something>`, which is how you tell handoff
channels from conversations in a long list. `⌘K` in the viewer filters on
purposes, so typing `handoff` finds them all.

Two things that are **not** signs of trouble:

- **Nobody is waiting.** After a handoff both agents stop. A conversation
  leaves an agent parked on a `saneha wait`; a handoff does not, and should not.
- **The channel stays open.** Nobody closes it when the doc is taken. That is
  deliberate: a closed channel refuses new joins, so closing it would stop a
  third session carrying the work on, and stop you asking a follow-up question
  in the viewer. Close it when the work is genuinely finished, or leave it.

## When something goes wrong

**"The new agent says there is nothing there."** The likeliest cause is the one
that bit us in testing. An agent's identity is derived from the repository, the
harness and the machine, so a fresh session on the same laptop, in the same
repo, under the same harness, gets the *same identity* as the session that
handed over — it resumes it, inheriting how far that session had read, which can
already be past the handoff. Tell it:

> Read the whole transcript: `saneha read <channel> --all`.

The current skill tells agents to do this without being asked. An agent that
does not is running an old copy — see below.

**"The old agent is still sitting there waiting."** It is on a skill from before
handoffs were written down, which told it to wait after every message. Update
it:

```sh
cargo install --path .    # in the saneha repo, if the binary is behind
saneha init               # rewrites the skill for every harness on this machine
```

`saneha init` says what it did for each harness it found — `updated`, or `up to
date` when the file already matches the binary. It only ever writes files saneha
itself installed, so nothing of yours is touched. **This is per machine** — a
second laptop needs its own `saneha init`, and a server redeploy does not do it,
because nothing serves the skill over HTTP.

**"It says the channel does not exist."** The receiving agent was given the name
before the other side created it. It will retry for a minute on its own; if it
still fails, check the name against `saneha list` or the viewer.

**"Fetching the attached file failed."** If the handoff came from the same
machine, the file is already sitting in that directory and `saneha fetch` will
not write over it. That is a refusal, not a loss — the copy on disk is the same
file.

## When not to bother

- **The next session is on this machine and the notes are already in the repo.**
  Then you have a file, and saneha is adding a record and a receipt rather than
  moving anything. Worth it when you want the trail; not worth it otherwise.
- **The two sessions are going to talk.** If the first one will still be around
  to answer questions, that is a conversation, and both sides should be in the
  wake loop. A handoff is specifically the case where one side is gone.

## Tidying up

Handoff channels accumulate: nothing in saneha expires, and one channel per
handoff adds up faster than one per project. `saneha list` shows them all, and
closed ones sink to their own group in the viewer.

```sh
saneha delete brisk-otter          # says what would go, and removes nothing
saneha delete brisk-otter --yes    # removes it, its transcript and its files
```

Deleting is asked twice on purpose and nothing brings a transcript back.
