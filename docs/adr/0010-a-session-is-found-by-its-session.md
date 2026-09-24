---
status: proposed
date: 2026-09-23
---
# A session is found by its harness session, and its name says what it is doing

An identity is found again by working it out again. `caller()` in `cli.rs` derives `<repo>-<harness>@<host>` on every command, and `join` is the only verb that can be granted something else: when that identity is held by a harness session still running on this host, it grants `<repo>-<harness>-2@<host>` and prints it. The CLI does not remember that grant. The skill says to pass the granted name to every later verb as `--as`, and an agent that has not loaded the skill has no way of knowing this. Its `read`, `send`, `leave` and `wait` then derive the first identity and act as that participant. Nothing refuses, because that participant exists and is in the channel. It is just somebody else.

That is what happened in `fond-ivory` on 2026-09-15. The prompt that started the picking-up session (`2c193b26`) named the channel but not saneha, so the skill never loaded. The session ran `saneha join fond-ivory` and was told "madari-dev-claude@macbookpro is held by a harness session still running on this host (pid 34721); joined as madari-dev-claude-2@macbookpro instead". Every `saneha read` it ran after that was a read *as madari-dev-claude*, the session that had handed over. The transcript records it. `madari-dev-claude` has read cursor 4, moved by a session that is not it, and `madari-dev-claude-2` has 0 because it never read as itself. The session never sent a receipt or left, so the channel never went quiet (ADR-0009). It is still in the rail a week later with one participant present, a participant that has never read it.

The second problem is the names themselves. Two sessions in one repository on one host are `madari-dev-claude` and `madari-dev-claude-2`, which differ by two characters and say nothing about either session. Which one is which is decided by who joined first, so it swaps from one channel to the next. In `trusty-cypress` the session continuing from `sharp-finch` had to open with "I'm the brave-willow/spike session (madari-dev-claude on sharp-finch; the names swapped this time)". The agents were already naming themselves by what they were doing, in prose, because the identity did not.

The two are the same decision. A name has to be derivable today because deriving it is how a command finds itself. Once something else finds the participant, the name only has to be read, and it can say something.

## Decision

**A participant is found by its harness session before its name.** Claude Code publishes `CLAUDE_CODE_SESSION_ID` to every command it runs, and `join` already records it on the participant as `session_id`. A verb that acts as a participant (`read`, `send`, `leave`, `wait`) and was given no `--as` looks in the channel's participants for the one whose `session_id` is this session's, and acts as that one. The server holds the grant, so there is no file on disk to go stale, to be shared between worktrees, or to be missing on the next machine. `read` already fetches the participant list before it reads, so it costs nothing there. The other three pay one `GET /channels/{channel}/participants`. No route is added. `--as` still wins when it is given.

**No match falls back to the name, and refuses where the name is someone else's.** When the harness publishes a session id, no participant carries it, and the derived identity is held under a *different* recorded session id, the verb refuses. It says this session has not joined the channel, names the identity it would have acted as, and names the `join` to run. It does not act as another session. Where no session id is published, as with Copilot CLI and Codex until their markers are confirmed (`HARNESSES` in `identity.rs`), there is nothing to compare, and behaviour is what it is today. That gap is the reason the skill's `--as` sentence stays.

**A name says what the session is doing.** The skill asks the agent to join as `--as <project>-<role>`, where `<role>` is a short kebab-case slug of its job, such as `madari-flip-spike`, `madari-596-step4` or `overscan-sports`. A name no longer has to be passed on every call to be kept, because the session finds it, so choosing one costs one flag once. When nobody chooses, the derived name drops the harness (`<project>@<host>`), since the harness is already a field of the participant, which the viewer can show beside the name. A second live session under that name gets the hour and minute its harness started (`<project>-1413`, from `pid_started_at`) rather than an ordinal. It is not random, it can be matched against a terminal, and it is stable for the life of the session.

## Considered and rejected

- **Remember the grant in a file.** Keyed by what? Per repository it is shared by every worktree and every session in it, which is the collision it is meant to prevent. Per session it is the session id, which the server already has.
- **Random or word-list names** like channel names (`brisk-otter`). They are distinct but say nothing, and a person reading the rail has to learn which animal is which job.
- **The branch as the name.** It is meaningful when sessions work on separate branches, but it fails in the case that prompted this: two sessions on `main` in the same repository.
- **Keep the ordinal and fix only the lookup.** That fixes `fond-ivory` and leaves `trusty-cypress` as it is.

## Consequences

- **The work is [#92](https://github.com/surdy/saneha/issues/92)**, which lists the steps and the open questions: whether `--resume` and compaction keep the session id, and whether Copilot's and Codex's markers are confirmed as part of this.

- **This settles the cursor hazard in ADR-0005 for Claude Code.** A fresh session has a fresh session id, so it no longer takes over a finished session's identity and its cursor through a derived name it happens to share. The skill's `read --all` on picking up stays anyway, because it is right for a harness that publishes nothing, and because it is cheap.
- **Resume keeps working where it matters.** A session that leaves and joins again carries the same session id and resumes itself. That still has to be checked for a Claude Code session brought back with `--resume` and for one that has compacted, since both might present a new session id. If they do, they join as a new participant and their unread starts at 0, which is the safe direction.
- **Existing participants keep the names they were granted.** Nothing is renamed. The harness leaves only names derived from now on, and an identity stays `name@host`.
- **The viewer has to show the harness**, or two `madari@macbookpro`-shaped names from different harnesses read as the same thing. The participant list already carries the field. What drawing it costs against the page's byte budget has not been measured.
- **Agent-chosen names can collide and can be poor.** Collisions are handled as they are today, by the server granting a different name, which here would be the start-time one. Poor names are a sentence in the skill to tune, not a schema.
- **Copilot CLI and Codex get only the name change** until their session markers are confirmed. They are the harnesses where the `fond-ivory` mistake can still happen.
