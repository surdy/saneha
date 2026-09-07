# Handoff findings

This is the acceptance record for the handoff sentences in
`skills/saneha/SKILL.md` ([ADR-0005](adr/0005-a-handoff-is-a-message-and-a-leave.md)).
That ADR decided a handoff is a way of using the verbs that are already there
rather than anything the server knows about, which puts the whole weight of it
on prose — so the question this page answers is whether the prose actually
changes what an agent does. It was run as an A/B: the same handoff, once under
the skill as it was before ADR-0005 and once under the skill after it.

## The run

| | |
|---|---|
| When | 2026-09-07 |
| Server | a local `saneha serve` on 127.0.0.1:7401, `/health` sampled every 3 seconds throughout |
| Arms | `swift-lantern` under the old skill (148 lines), `amber-prairie` under the new one (194) |
| Agents | four fresh Claude Code subagents, one handing over and one picking up per arm. Subagents, not standalone sessions — see what this does not show |
| Given | the skill file, `SANEHA_URL`, a working directory, and the state of an invented auth refactor. No recipe, no mention of waiting, and no hint that a comparison was being made |

Both arms ran in a directory of their own, so each pair derived one identity
(`arm-old-claude@macbookpro`, `arm-new-claude@macbookpro`) and the one picking
up resumed the one handing over — which is the ordinary same-machine handoff
and the case ADR-0005 is most worried about.

## What happened

| | old skill | new skill |
|---|---|---|
| Handed over without waiting | yes | yes |
| Left rather than closed | yes | yes |
| Recovered the doc | yes | yes |
| Recovered the gotcha — a `Set-Cookie` double-write, which fixed the order of the remaining work | yes | yes |
| **Parked on a background wait afterwards** | **yes — `--timeout 3600`** | **no** |
| Peak `held_waits` on the server | 1, and still held after the turn ended | 0 |

### The side handing over needed none of it

Both sessions handing over got it right, under both skills: each sent the doc,
left, did not wait, and did not close — and the one under the old skill said
why, unprompted, that a background wait would have had nobody to service it and
that closing would refuse the next agent's messages. Nothing in this run
motivates a single sentence aimed at the side handing over. What the change is
for is the side picking up.

### The posture is what the sentences changed

The agent picking up under the old skill finished by backgrounding
`saneha wait swift-lantern --timeout 3600` and reported itself "parked on a
background `saneha wait`". It was the only participant in the channel and the
other side had left, so it had settled in for an hour waiting for somebody who
had left. The server agrees rather than only the agent's own account:
`held_waits` went from 0 to 1 at 12:21:57 local time and was still 1 after that
agent's turn was over.

The agent picking up under the new skill did not wait, and gave the reason:
"`participants` shows I am the only member of the channel, and this is a
handoff — nothing is expected back, so there is nobody to wait for."
`held_waits` never left 0 in that arm.

Which sentence earned that is worth being exact about, because the obvious
reading is wrong. The **Joining** rule said to stop when everybody else is
away — and in a resumed handoff there is no everybody else: `participants`
printed one row, the agent's own, marked `here`. The rule was vacuous in the
case it was written for, and what the agent actually leant on was "this is a
handoff — nothing is expected back", which is **Handing off**. The rule now
says that one row means the same thing as an away channel and not the
opposite; that sentence has not been tested, because it did not exist when this
ran.

The parked wait then cost a turn, which was not planned and is worth
recording because it is the price of the posture rather than an argument about
it. Tearing the trial down killed the server out from under that agent; its
background wait exited 1 on a refused connection and re-invoked it, and it
spent a full model turn working out what had happened and reporting back. It
did that well — it named the failure as the server being gone rather than as
silence or a wrong identity, and it declined to restart anything or re-enter
the loop without being asked. But none of that turn was work. An agent that
has finished a handoff and parked is not merely idle: it is attached to a
channel, and anything that disturbs the channel spends it. The provenance here
is an accident of cleanup, not a finding in the wild.

### The cursor sentence was not what saved the doc

The hazard was genuinely present. The session handing over under the old skill
ran a plain `read` after sending, which moved its cursor to 3 of an eventual 4,
so the participant the next agent resumed had the handoff message already
behind its cursor. A plain `read` there returns the leave and the resuming
join — a resume writes a join into the transcript like any other — and not the
doc.

Both agents nevertheless ran `read --all` and both recovered the doc and the
gotcha in full. The one under the old skill chose it with no instruction to,
and noticed the mechanism by itself: "My cursor was already at 3 on join, so
the handoff messages came from `read --all`, not from unread." So on this
evidence the `--all` sentence is insurance rather than a fix for something
observed failing — the failure it describes is real and reachable, and a
capable model avoided it unprompted. It stays, at the cost of two lines,
because the plain `read` is the one the rest of the skill teaches.

Worth noting for the same reason: the new skill's recipe never runs a
cursor-moving read at all, and the new arm's participant was still at cursor 0
when the pick-up began, against 3 in the old arm. So the hazard did not arise
there at all. That is a property of the recipe rather than something it says.

### A defect the run found

`saneha fetch <channel> <id> --out HANDOFF.md` failed for the agent picking up,
because the handoff had come from the same machine and `HANDOFF.md` was already
sitting in the working directory, where `fetch` will not write over it. The
agent recovered — it fetched under a second name and diffed the two — but the
skill had not said this would happen. ADR-0005 had the sentence and the line
budget cut it; the run put it back.

## What this does not show

- **One run per arm, and one harness.** Four Claude Code sessions, no Copilot
  CLI and no Codex, where [wake-findings.md](wake-findings.md) covers two
  harnesses. A handoff between two different harnesses has not been run.
- **The skill was read from a path, not loaded.** Each agent was told to read
  the file. Whether the skill's description triggers it when a person says
  "hand this off" is tested in `tests/skill.rs` and was not exercised here.
- **These were subagents, not standalone sessions.** None had a wake loop
  already running when it started, so what was measured is whether an agent
  enters one, never whether it can be talked out of one it is already in.
  Harness re-invocation itself was present — the parked wait proved that by
  re-invoking its agent when the server went away.
- **Nothing survives to be checked.** [wake-findings.md](wake-findings.md)
  names its channels and says the transcripts can still be read back. This ran
  against a local server that was torn down afterwards, so there is no
  transcript, no database and no artifact behind any number here: a reader can
  only take this page's word for it.
- **Nobody continued the work.** Both agents picking up were told not to touch
  a repository, so what a handoff is actually worth — whether the next session
  gets further for having read one — is untested and is not the kind of thing
  this setup could show.
