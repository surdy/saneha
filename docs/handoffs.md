# Handing work between agents

A saneha channel is usually a conversation: two agents talking while they work
on something together. A **handoff** is the other reason to open one. A session
is ending — its context is full, or you are stopping for the night — and you
want a fresh agent to carry the same work on. The one that is leaving writes
down where things stand, and the one that arrives picks it up. Nothing is
expected back.

This page is for the person running the agents. What the agents themselves do
is in [the skill](../skills/saneha/SKILL.md); why it works this way rather than
with a `--handoff` flag is [ADR-0005](adr/0005-a-handoff-is-a-message-and-a-leave.md),
and why a taken handoff drops out of your channel list without anything being
closed is [ADR-0009](adr/0009-quiet-is-a-view-of-the-participants.md).

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

`SANEHA_URL` is how the CLI finds the server, and `saneha init --url
https://saneha.clusterfault.com` saves it so you do not have to set it again —
on this machine, for you and for every agent on it. Do not use a shell profile
for this instead: a harness runs each command in a fresh shell that may never
read one, which is why an agent could say it has no server on a machine where
the address works perfectly well by hand. If one still says so, that machine
has not been `init`ed; tell it the address and it will prefix its own
commands.

### Naming it yourself

If you would rather not copy a minted name around, make the channel first — the
**new** button in the viewer's channel list takes a name and a purpose — and
give the same name to both sessions:

> Hand this off through saneha in `auth-refactor`.

A channel the handing-over agent is told about is one it joins rather than
mints, so the name is one you chose and can remember.

## What you should see

In the viewer, a handoff is a short transcript: a join, one message with the
state of the work, a leave, then later another join, a line saying it has been
taken, and a second leave. The message is the handoff — it is in the transcript
rather than in an attachment, so you can read it yourself without downloading
anything.

That second leave is what tidies up after itself. Both sides have now left, so
nobody is present, and the viewer folds a channel nobody is in under **Quiet**,
the same way it folds the closed ones — shut until you click the heading, which
your browser then remembers. So a taken handoff leaves your channel list on its
own, without anything being closed and without you deciding anything. A channel
someone is still waiting in never goes quiet, because a participant parked on
`saneha wait` has not left.

If you posted into the channel yourself, you are a participant too, and the
channel stays out of the quiet pile until you leave as well — **leave channel**
in the row's menu, which is reversible: posting again joins you again.

The purpose line reads `handoff: <something>`, which is how you tell handoff
channels from conversations in a long list. `⌘K` in the viewer filters on
purposes, so typing `handoff` finds them all.

Two things that are **not** signs of trouble:

- **Nobody is waiting.** After a handoff both agents stop. A conversation
  leaves an agent parked on a `saneha wait`; a handoff does not, and should not.
- **The channel stays open.** Nobody closes it when the doc is taken. That is
  deliberate: a closed channel refuses new joins, so closing it would stop a
  third session carrying the work on, and stop you asking a follow-up question
  in the viewer. Close it when the work is genuinely finished, or leave it —
  quiet is what keeps it out of your way in the meantime, and one join brings
  it back with nothing to undo.

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
handoffs were written down, which told it to wait after every message. From now
on a join says so on standard error — see *Keeping up* — but not on a machine
whose binary predates that too, which is the machine this happens on. Update it:

```sh
cargo install --path .    # in the saneha repo: this is what carries the change
saneha init               # only needed the first time on a machine
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

## Keeping up

The instructions your agents follow are not kept in a file on your machine.
They are in the `saneha` binary, and the file `saneha init` puts in each
harness's skills directory is twenty-one lines saying so — it tells the agent to
run `saneha skill` and follow what that prints. So **updating the binary updates
what your agents are told**, and there is nothing to re-copy afterwards.

That leaves `saneha init` as a first-run step per machine. Run it once, and
again only if you are told to; a change to the line that decides *when* a
harness loads the skill is the one thing the binary cannot deliver on its own.

`saneha join` is what tells you, on standard error, and there are two messages:

- **"the saneha pointer installed for Claude Code is behind this binary"** —
  the file in that harness's directory is not the one this binary would write.
  Since that file is now a fixed twenty-one lines, this is rare and means the
  frontmatter moved. `saneha init` fixes it, and names the harness so you do not
  have to go looking.
- **"this binary and the server were built from different saneha skills"** —
  the two are out of step. It does not say which is behind, because it cannot:
  a skill change and the deploy that ships it are separate pull requests, so
  for a while after every skill change anyone who has built from `main` is
  *ahead* of the server. You can tell which way round it is; the command
  cannot.

If it is your binary that is behind, **`saneha init` does not fix it** and is
worse than useless — with an old binary it reinstalls the old skill and reports
`up to date`. Update the binary, then run `init`.

That second message is why this exists. A machine can sit for weeks on a binary
from before a change, reporting success the whole time, and nothing would say
otherwise. A server too old to report its skill at all says nothing, which is
right: it is the older of the two, so the skill you hold is the newer one.

`saneha init` is idempotent, needs no server and runs in a few milliseconds, so
if you would rather not think about even the first-run step, it is fine in a
shell profile:

```sh
saneha init >/dev/null 2>&1
```

It cannot tell you the binary is old, and that is now the only staleness left
that matters — only the join warning says so.

## When not to bother

- **The next session is on this machine and the notes are already in the repo.**
  Then you have a file, and saneha is adding a record and a receipt rather than
  moving anything. Worth it when you want the trail; not worth it otherwise.
- **The two sessions are going to talk.** If the first one will still be around
  to answer questions, that is a conversation, and both sides should be in the
  wake loop. A handoff is specifically the case where one side is gone.

## Tidying up

Handoff channels accumulate: nothing in saneha expires, and one channel per
handoff adds up faster than one per project. The viewer keeps the taken ones
out of sight on its own — they are quiet, since nobody is in them — so this is
about the disk and about `saneha list`, which shows them all.

If you hand off often, the other half of the answer is to stop minting a
channel each time. Name one and use it for every handoff in a repository:

> Hand this off through saneha in `saneha-handoffs`.

An agent told a name joins that channel rather than minting one, so the list
stops growing, the name is one you never have to carry again, and the
transcript becomes that repository's handoff log. What you give up is the clean
sweep: one transcript holds hops of unrelated work, and whoever picks up reads
the lot.

To remove one for good:

```sh
saneha delete brisk-otter          # says what would go, and removes nothing
saneha delete brisk-otter --yes    # removes it, its transcript and its files
```

Deleting is asked twice on purpose and nothing brings a transcript back.
