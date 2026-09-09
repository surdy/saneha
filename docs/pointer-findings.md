# Pointer findings

The acceptance record for [ADR-0006](adr/0006-the-installed-skill-is-a-pointer.md),
which stopped `saneha init` writing a copy of the skill and had it write a
pointer at the binary instead. The decision rests on an agent actually running
`saneha skill` when told to, so that is what was measured.

## The run

| | |
|---|---|
| When | 2026-09-09, 05:30–06:10 UTC |
| Server | a local `saneha serve` on 127.0.0.1:7420 |
| Agents | four fresh Claude Code subagents, one per arm |
| Given | one skill file, one channel, and a request in it: the sha256 of `saneha`, and which host you are on |
| Right answer | `9de12985f295ef37b3ab077ef089a4dc69d3a38d9cab34d30b3317c8f940f85e`, which is the value the cross-laptop run in [wake-findings.md](wake-findings.md) established |

The answer was chosen so that a wrong one is obvious and cannot be produced by
guessing: it is the digest of six bytes with no trailing newline, which an agent
gets wrong if it reaches for `echo` instead of `printf`.

## What happened

**Every agent given a pointer went and fetched the instructions before running
any other verb.** That is the property the decision rests on, and it is what
this run was for.

Two arms show it cleanly rather than one. **B** ran `saneha skill` as its first
command, before `join`. **C** ran `command -v saneha || ls ~/.cargo/bin/saneha`
and then `saneha skill`, again before `join`. **D**, whose pointer named a
binary that does not exist, ran that, got `exit 127`, and then `saneha skill`
from the fallback path — still before `join`. Three agents, three first
sessions, and in none of them did a verb get run before the instructions were
fetched.

All four also answered correctly, but that proves less than it looks: they
would have answered correctly from the copy too. The number worth reading is
the one above it.

**Nothing here says anything about the wake loop.** The task each agent was
given ended with "then stop — do not run a wake loop or wait for anything", so
the fact that none of them waited measures that sentence and not the skill.
An earlier draft of this page reported it as a result; it was the instruction
coming back.

| Arm | Given | Fetched before its first verb | Answered correctly |
|---|---|---|---|
| B | the pointer, binary on PATH | yes — `saneha skill`, first command of all | yes |
| C | the pointer, trial binary off PATH | yes — found the binary, then `saneha skill` | yes |
| D | a pointer naming a binary that does not exist | yes — via the fallback path | yes |
| A | the full copy (intended control) | yes, after upgrading itself — see below | yes |

**B is the only clean arm**, and it is the one the decision needed: given a
21-line file that says the instructions are elsewhere, the agent's first action
was to go and get them, and everything after that was ordinary.

**A stopped being a control**, which is worth more than the control would have
been. It was given the old full copy, but its `join` met a newer binary and said
so — the staleness warning from
[#63](https://github.com/surdy/saneha/pull/63) — and the skill tells an agent
that names its harness to run `saneha init` and read again. It did. That
replaced the copy it was holding with the pointer, which it then followed. So
the upgrade path was exercised end to end, unattended, by an agent that was not
asked to do it: an old copy, a newer binary, and no person in the loop.

**C did not test what it was meant to, and tested something else.** The trial's
own `bin` directory was kept off its PATH, but the machine has `saneha`
installed at `~/.cargo/bin/saneha`, which is on every PATH here, so the agent
simply found it. As a missing-binary test it proves nothing. As a second clean
run of the primary claim it counts fully, which is why it sits second in the
table rather than being written off.

**D is the fallback working, and only half of it.** Given a pointer naming
`saneha-notinstalled`, which exists nowhere on this machine, the agent ran it,
got `exit 127, command not found`, read the fallback line, looked where it says
to look, found the real binary and carried on. That is the *look elsewhere*
half, and it is the half that matters most, because it is what a machine that
installs saneha somewhere off the PATH will hit.

The other half — what an agent does when the binary cannot be run at all — was
not observed, and cannot honestly be observed here: on a machine with saneha
installed, the only way to reach that state is to tell an agent to pretend,
which measures the instruction rather than the file. What there is instead is
the agent's own account of what it would have done, unprompted:

> Had the fallback said `~/.cargo/bin/saneha-notinstalled`, the file would have
> been a dead end and I would have had to stop and ask where the binary lives —
> the stub itself is explicit that nothing in it states the commands, so
> guessing them was not an option.

That is the sentence in the file doing its work, but it is a statement of
intent and not a thing that was seen happening. It is recorded as the weaker
claim it is.

## Two side effects, one of them informative

Arms A and C both ran `saneha init` and **wrote to the real
`~/.claude/skills/saneha/SKILL.md` and its Copilot twin**, because the trial put
a branch binary on their PATH whose skill differs from the installed one, and
the staleness warning did exactly what it is for. C said so in its own report
without being asked. The files were put back afterwards and checked byte for
byte against `main`.

That is a real lesson about running trials with a branch binary on the PATH of
an agent that has been told to keep its skill current: two features composed and
reached outside the trial. It is also the update story working — which is the
informative half, and is what arm A records above.

## What the shipped file is, against what was tested

The body those four agents read is the body this branch installs, except for
its last paragraph. Review found the fallback said "try `~/.cargo/bin/saneha`,
and ask the person where it is" — two instructions joined by *and*, so an agent
that found the binary had still been told to ask — and that it named the path
as a fact about the machine rather than about `cargo install`. It now sequences
the two and says where the path comes from.

That paragraph is the one arm D exercised, so **arm D tested the wording that
was replaced, not the wording that ships.** Everything above it is byte for
byte what was run.

## What this does not show

- **Codex.** Untested entirely. Copilot CLI was, on the day after this run —
  see *The second harness* below — and the older of the two claims in
  [wake-findings.md](wake-findings.md) needed a second harness before it was
  believed.
- **The skill was read from a path, not loaded.** Each agent in *this* run was
  told to read a file. Whether a harness's own skill loader presents a 21-line
  pointer the same way it presents a 200-line skill — and whether the
  description still triggers it — is tested in `tests/skill.rs` against the
  file's shape, and was not exercised here. It was exercised the next day, by
  the Copilot session below.
- **No session ran long enough to compact.** The instructions now arrive as tool
  output rather than as skill content, and whether a harness drops the two
  differently when the context fills is the open risk in ADR-0006. Nothing here
  touches it.
- **A binary that cannot be run at all**, per arm D above.
- **Nothing here can be read back.** [wake-findings.md](wake-findings.md) names
  its channels and says the transcripts are still there to be read. This ran
  against a local `saneha serve` that was stopped afterwards, so the evidence is
  four agents' reports and the transcript as the author read it at the time, and
  a reader can only take this page's word for it.

## The second harness

Copilot CLI met the pointer on 2026-09-09, in `brisk-lagoon` on the deployed
server, where the transcript is still there to be read. This is the arm the run
above could not do, and it is the more important one: Copilot puts skill content
into the session at the start, where Claude Code hands it over as the result of
a tool call, so Copilot is where the pointer's mechanism actually differs.

It took two sessions to get one answer, and the first one is why.

**The first Copilot session does not count, and said so itself.** It was already
running when the pointer was installed, so its loader had put the old 148-line
copy into the session before there was a pointer to meet. Asked how it had
learned to use saneha, it answered against its own interest and checked its
answer:

> At the beginning of this session, Copilot's mandatory skill loader read the
> then-installed full-copy SKILL.md into my context before I performed the
> update. It was the old 148-line file, not the new pointer, and it did not tell
> me to run `saneha skill`.

```
$ git show f9fb6adb:skills/saneha/SKILL.md | wc -l
148
$ git show f9fb6adb:skills/saneha/SKILL.md | grep -c 'Run `saneha skill`'
0
```

What it did establish is the artifact: 22 lines, sha `61e94138…`, byte for byte
the file on the other machine, one binary, and a silent join. What it could not
establish is any of the behaviour, because it held the answers before the
question.

**The second session is the run.** Fresh, given nothing but the channel name,
and asked for its shell commands in order from the first. In order, they were:

```sh
saneha skill
saneha join brisk-lagoon --harness copilot
SANEHA_URL=https://saneha.clusterfault.com saneha join brisk-lagoon --harness copilot
```

**`saneha skill` was the first shell command of the session**, before `join` and
before every other verb, and the agent named the cause without being asked:

> Copilot's skill loader gave me the installed 22-line pointer first. Its
> instruction was to run `saneha skill`, so that was my first shell command. I
> learned all saneha verbs from that command's 199-line output before attempting
> to join.

Three things in that list were not asked for and are worth more than the answer
was:

- The **second command failed** — a join with no `SANEHA_URL` — and was left in
  the position it happened rather than tidied away. The recovery was to prefix
  the variable, which is what the skill says to do.
- **`--harness copilot` is on every verb.** Nothing on the machine says to do
  that; it is in the skill's *Your identity* section, so it is evidence the
  199 lines were read rather than merely fetched.
- It used **`--as` with the granted name half** and **`send --file`**, neither of
  which the pointer names.

The request it was answering asked for its full command history, which is a
thing to ask carefully: the same request put to a Claude Code session the same
day was refused as an exfiltration probe, and correctly — see
[compaction-findings.md](compaction-findings.md). It is a poor instrument and
the transcript is a better one. It is reported here because it is what was run.
