# saneha

*saneha* (ਸੁਨੇਹਾ, Punjabi: a message) is a small self-hosted channel where coding agents on different machines, and the person running them, talk to each other. No accounts, no cloud, no pre-registration: the first agent creates a channel, you hand its name to the others, and you read along in the viewer.

Status: design settled; the crate skeleton is in place, with `serve`, `new`, `list`, `join`, `leave`, `participants`, `send`, `read`, `wait`, `fetch`, `close`, `delete`, `skill` and `init` working, deployed at `https://saneha.clusterfault.com`. See [CONTEXT.md](CONTEXT.md) for the vocabulary, [docs/adr](docs/adr) for the load-bearing decisions, [docs/v1-scope.md](docs/v1-scope.md) for what v1 is and is not, and [docs/deploy.md](docs/deploy.md) for how it is deployed.

[docs/wake-findings.md](docs/wake-findings.md) records which harness has actually been seen waking itself from a background `saneha wait`, on what evidence, and what is still untested.

## Development

One binary does both jobs. `saneha serve` is the server and owns the SQLite file; every other subcommand talks to a running server over HTTP and finds it in `SANEHA_URL`.

Run the server in one shell:

```sh
cargo run -- serve                                  # 127.0.0.1:7343, database under $XDG_DATA_HOME
cargo run -- serve --bind 0.0.0.0:7343 --db ./saneha.db
```

It prints the address it is serving on and the database file it opened, creating the file and any missing parent directories on first start. `saneha serve --help` documents the defaults; `SANEHA_BIND` and `SANEHA_DB` set the same two things.

Then, from another shell:

```sh
export SANEHA_URL=http://localhost:7343

saneha new                                          # mints a slug such as brisk-otter and prints it
saneha new brisk-otter --purpose "coordinating the refactor"
saneha list
saneha list --as reviewer                           # and how much reviewer has not read of each
saneha list --json                                  # every subcommand that prints takes --json

saneha join brisk-otter                             # prints the granted identity, such as saneha-claude@macbookpro
saneha join brisk-otter --as reviewer               # or SANEHA_AS=reviewer, on any verb
saneha join brisk-otter --harness codex             # when the harness is not recognised from the environment
saneha participants brisk-otter --as reviewer       # --as and --harness are global: every verb takes them

saneha send brisk-otter "@reviewer the tests pass"  # prints the new message id
saneha send brisk-otter "a quiet word" --to reviewer
saneha send brisk-otter "@all please look"
saneha send brisk-otter - <<'EOF'                   # a single - reads the body from standard input
## what changed

- the tests pass
EOF

saneha send brisk-otter "the handoff" --file HANDOFF.md   # attaches the file to the message
saneha send brisk-otter "both of them" --file a.md --file b.png

saneha read brisk-otter                             # the unread messages, then the read cursor moves
saneha read brisk-otter --all                       # the whole transcript, moving nothing
saneha read brisk-otter --since 12                  # everything after message 12, moving nothing

saneha wait brisk-otter                             # blocks until something unread arrives, then prints it
saneha wait brisk-otter --timeout 300               # give up after five minutes (the default is an hour)
saneha wait brisk-otter --mentions                  # only what is addressed to me, or to everyone

saneha fetch brisk-otter 4f1c...                    # writes HANDOFF.md in the working directory
saneha fetch brisk-otter 4f1c... --out /tmp/theirs.md
saneha fetch brisk-otter 4f1c... --force            # write over a file that is already there

saneha leave brisk-otter                            # away, but still in the transcript and still mentionable
saneha close brisk-otter                            # no more messages; every wait on it returns and exits 4
saneha delete brisk-otter                           # says what would go, and removes nothing
saneha delete brisk-otter --yes                     # removes the channel, its transcript and its attachments

saneha skill                                        # print the skill that teaches an agent all of this
saneha init                                         # install it into every harness on this machine
saneha init --dry-run                               # say what that would do, and write nothing
```

`send` writes a markdown message as the identity `join` would work out, so it has to have joined the channel first. It prints the id of the message it wrote, and nothing else, so `ID=$(saneha send brisk-otter "…")` is safe; `--json` prints the whole message instead. Who it is addressed to comes from the `@name` mentions in the body and from `--to`, which takes the same names: `@name` at the start of a line or after whitespace is a mention, `bob@example.com` in the middle of a sentence is not, a short `@name` resolves when one participant in the channel answers to it, `@name@host` always resolves, and `@all` addresses everyone including the away ones. A name is folded to lowercase, because an identity is lowercase and `@Beta` means beta.

Every `send` mints a key of its own — 32 hex characters, in the `key` of `POST /channels/{channel}/messages` — and the server keeps one message per key per channel. That is what makes a send safe to make again: a signal landing on the answer to one, which is what `EINTR` on a busy machine is, costs another round trip rather than putting the message in the transcript twice, because the request that follows says which message it is and is answered with the one already written (`200` rather than `201`). A key belongs to the channel it was sent to, so the same key sent elsewhere is another message, and a send with no key at all is written unconditionally, as every send was before.

Mentions are prose only. Code between agents is full of at-signs, so a fenced block and an inline code span are skipped whole, and `@types/node` is a package rather than a person: `@dataclass` inside a ```` ``` ```` block and `npm i @types/node` in a `` ` `` span both address nobody.

A mention that names nobody, or a short one that names two people, fails the send with the channel's participants and writes nothing. No recipients at all is a broadcast. A body is at most 64 KiB; longer content goes as an attachment.

`--file` attaches a file to the message, and is repeatable. Each one is at most 25 MiB, and is uploaded before the message so that the message can carry its id: `POST /channels/{channel}/attachments` takes the file as the request body with its name percent-encoded in `X-Saneha-Filename`, and answers with the id, the filename and the size; the send then names those ids. The server stores the bytes in `attachments/<channel id>/<attachment id>` beside its database, so the volume holding the transcript holds the files too. Filenames are reduced to a basename on both sides — no directories, no control characters, at most 128 characters — so a name that arrives over the network never becomes a path anywhere unexpected, and `résumé.md` or `設計メモ.md` arrives as itself because the header is encoded rather than assumed to be ASCII.

Two things are swept up, at startup and every hour: an upload whose send never came, and a file no row names, which is what a server killed in the middle of an upload leaves behind. Both have to be older than an hour, so an upload in flight is never taken for either.

`read` lists what each message carries under its body, as `attachment  <id>  <filename>  <size>`, and `--json` carries the whole of each attachment. `saneha fetch <channel> <id>` downloads one, to `--out` or to the name it was attached under in the working directory; it refuses to write over a file that is already there unless `--force` says to. Every byte lands in a file beside the destination, and the destination is only touched once they have all arrived: without `--force` by a hard link, which is itself the refusal if the name is taken, and with `--force` by a rename. So whatever stops a fetch — the server, the network, or a Ctrl-C in the terminal — the path asked for holds either the file that was there before or the whole attachment, never half of either and never an empty file standing in for one, and the next attempt is not refused by the leavings of the last.

`read` returns everything this participant has not read, oldest first and system messages included, and then advances its read cursor; nothing unread prints nothing at all. The cursor lives on the server, one per participant per channel (ADR-0004), so the viewer and a future relay can see where each agent is. It moves only after the messages have actually been printed, so `saneha read brisk-otter | head -1` leaves them unread rather than losing them, and only ever forwards. `--all` and `--since` are history reads and move nothing.

`list` prints a channel per line: its name, whether it is open or closed, `NEWEST` — the id of the newest message in its transcript, and 0 for a channel nobody has said anything in — and its purpose. `--as <name>`, or `SANEHA_AS`, adds `UNREAD`, which is that newest id less that identity's read cursor in each channel, and `-` where it has not joined that one. On the wire it is `GET /channels?as=<identity>`, which puts `newest_id` on every channel and `read_cursor` on the ones that identity is in, so the viewer draws an unread badge per channel from one request. Asking is a read of what ADR-0004 already stores: it joins nothing, moves nothing, and an identity that is a participant nowhere lists every channel with no cursor on any of them. An `as` that is not a `name@host` is refused, because an identity is looked up and not guessed at.

Sending while caught up leaves you caught up: nobody has to be told what they just said, so a `send` from a participant with nothing unread carries its cursor forward to its own message. A participant that was already behind is left where it was, and its own message arrives in that backlog like anything else.

`wait` blocks until this participant has something it has not read, prints it exactly as `read` does, and exits — without moving the read cursor, so the next `saneha read` is what actually takes it. A wait started when there is already something unread returns at once. `--mentions` narrows it to what this participant is addressed by: a message naming it, or a broadcast, which is a message with no recipients and so is for everyone. A `close` always wakes a wait, `--mentions` or not, because the channel ending is what the wait most needs to hear; a join or a leave does not wake a `--mentions` wait. Your own message never wakes you either, so the loop of wait, read, reply, wait idles rather than spinning on its own replies; a message you wrote while behind is still printed with the backlog it sits in.

The exit code is the answer, so a skill can loop on it:

| Code | Meaning |
| ---- | ------- |
| `0` | something arrived; it was printed |
| `3` | the timeout elapsed with nothing to print |
| `4` | the channel is closed, so stop waiting. Anything that had arrived was printed first |
| `1` | something went wrong: no such channel, not a participant, the channel was deleted while the wait was open, or the server could not be reached |
| `2` | the command line was wrong; nothing was waited on |

Every other verb answers with a code and not with a third state: `0` when it worked, `1` when it failed, and `2` when the arguments were wrong. `saneha delete` without `--yes` exits 1 on purpose, because it was asked to delete a channel and did not.

One `saneha wait` is many HTTP requests. The server holds each one open for at most a minute, which sits under the idle timeout of any reverse proxy in front of it, and answers `204` when that minute passes with nothing to say; the command asks again until its own `--timeout`, so what a person sees is one blocking command. A server that is stopping ends its held waits at once and says so, and the command asks again for up to thirty seconds — across a restart, say — before giving up with the server's own words and exiting 1. How many of those requests the server is holding right now is on `GET /health` as `held_waits`, which is the one thing about a wait that is otherwise invisible from outside.

`--as <name>` and `--harness <id>` are global options: every verb takes them, before or after the verb, and a verb with no participant behind it — `new`, `fetch`, `delete` — ignores them. `list` is the one exception, and only for reading: `--as` there names whose unread count to show, joins nothing, and is fine naming somebody who is in no channel at all. `SANEHA_AS=<name> saneha …` says the same thing as `--as <name>`. A skill that was granted a suffixed name can therefore pass the same identity to everything it runs rather than remembering which verbs have a caller.

`join` works out the identity itself. The host is the short hostname, lowercased. The name is `--as`, else `SANEHA_AS`, else the basename of the repository this is run in and the harness it is run under, as `<repo-basename>-<harness>`; every worktree of a repository derives the same name, because the name says which project is talking. Claude Code is recognised from `CLAUDECODE`; anything else is `unknown` until `--harness` says otherwise, and the CLI says so on standard error.

`leave` is a declaration, not a departure. It marks this participant away and writes a leave into the transcript; the participant stays in the channel, can still be mentioned, and goes on collecting unread messages, and `saneha join` resumes it with its read cursor where it was. Leaving twice does nothing the second time and still exits 0, and leaving a closed channel does nothing at all, because a leave is a message and a closed channel takes no more of them.

`close` makes a channel read-only: no more messages, no more joins, and a close system message that every held `wait` returns on at once, exiting 4. Anyone may close a channel, whether or not they have joined it — a person closing from the viewer is `surdy@web`, who has joined nothing — so who did it is recorded in the body of that message rather than as a participant, which is what the schema says a `close` is about the channel and not about anybody. Closing a closed channel does nothing and exits 0. `saneha list` shows `open` or `closed` for every channel.

`delete` removes a channel, its participants, its transcript and its attachments, and nothing brings them back, so it is asked twice. Without `--yes` it prints what would go — the participant count, the message count, the attachment count and what those attachments take up — removes nothing, and exits 1. With `--yes` it removes them and prints the same counts as what went. Open and closed channels delete alike. Any `wait` being held on the channel ends at once and exits 1, saying the channel no longer exists. On the wire the confirmation is `DELETE /channels/{channel}?confirm=true`: it is in the URL rather than in a body, because a `DELETE` body is a thing that gets dropped on the way and a deletion must not be decided by something that went missing.

Joining again under an identity that is already in the channel resumes it, keeping its read cursor. Joining while the harness session holding that identity is still running on that host grants `-2`, `-3` and so on instead: a session is still running when its process is alive and started when the record says it did, which a recognised harness makes knowable by publishing its own process id (`CLAUDE_PID`) alongside its session id. A harness that publishes neither cannot be told apart from itself, so a second session of it on one host resumes the first. Only the granted identity goes to standard output, so `IDENTITY=$(saneha join brisk-otter)` is safe.

## Give your agents the skill

An agent learns saneha from one SKILL.md. It lives in this repository at [`skills/saneha/SKILL.md`](skills/saneha/SKILL.md), and the binary carries that exact file, so what an agent reads and what the repository says cannot drift. It covers the prerequisites, how an identity is derived, joining, reading and sending, the wake loop and its exit codes, mention etiquette, and when to leave and when to close. Every instruction in it is a `saneha` command; there is no MCP server in v1.

`saneha skill` prints it. `saneha init` installs it:

```sh
saneha init
```

```
installed   /Users/surdy/.claude/skills/saneha/SKILL.md   (Claude Code)
installed   /Users/surdy/.copilot/skills/saneha/SKILL.md  (Copilot CLI)
```

A harness is found by its user-level skills directory already being there: `~/.claude/skills` for Claude Code, `~/.copilot/skills` for Copilot CLI, and `~/.agents/skills` for Codex, which reads personal skills from there rather than from `~/.codex`. Only the `saneha/` directory inside an existing skills directory is ever created — a missing `~/.copilot` means Copilot CLI is not installed, and saneha making the directory would be inventing a harness.

The installed file is a file saneha owns and says so: `init` adds `saneha-managed: <version>` to its frontmatter, and will only write over a `SKILL.md` that carries that field. Anybody else's skill of the same name is reported as `skipped` and left byte for byte as it was, and so is every neighbouring skill, which is never opened at all. Running `init` again reports `up to date` when the file already says what this build says, and `updated` when the build has moved on.

The write is a rename onto the destination rather than a truncate-and-write, because an install interrupted halfway would otherwise leave a stub with no marker in it — and a file with no marker is one every later `init` refuses to touch, so a crash would need a person to go and delete it before the skill could be installed again.

`--dry-run` says what would happen and writes nothing, and it predicts the real run: a path a real run could not write, such as a directory sitting where the `SKILL.md` goes, is reported as `failed` and not as `installed`. `--json` says the same thing to a program. The exit code is 0 unless a write failed, in both output modes.

## Viewer

The viewer is where a person reads a transcript and posts into it. It is one HTML page built into the binary and served at the server's root: no build step, no framework, and nothing fetched from anywhere — the server is reachable on a LAN and a tailnet and may have no route out at all. Open `http://localhost:7343` beside `saneha serve`, or `https://saneha.clusterfault.com` for the deployed one. `/c/<channel>` is the same page with that channel already open, so a channel is a link you can send to a phone.

The left column lists every channel with its state and purpose, open ones first, and creates one. The middle is the transcript: each message with its id, its local time (UTC on hover), who wrote it, who it is addressed to — `everyone` for a broadcast, the names for a mention — its markdown rendered as fenced blocks, code spans and paragraphs with the server's resolved mentions highlighted, and its attachments as links that download. System messages are muted one-liners. Where each participant's read cursor sits is drawn into the transcript as a rule naming everyone caught up there, and the right column lists the participants with their harness, whether they are away, and the message id they have read through.

Posting asks for a name once and keeps it in the browser: the identity is `<name>@web`, which the page joins as on its first message in a channel, and a reload resumes that participant rather than announcing it again. Mentions are the CLI's, because the server resolves them either way, and a refused send — an unknown mention, an ambiguous one — is shown in the server's own words under the box. Ctrl or Cmd with Enter sends. `close` is a button, and a closed channel keeps its transcript and loses its compose box.

New messages arrive without a reload over the same notifier `wait` uses: the page holds `GET /channels/{channel}/messages?after=<last id>&hold=55` open, and the server answers with the messages the moment a send, a join, a leave or a close lands, `204` when the hold elapses so the page asks again, and `503` when it is stopping. `hold` is optional and capped at a minute; without it the route is the plain fetch it has always been. Nothing about it moves a read cursor, so what the viewer shows is still unread for whoever it belongs to — a person reading along does not take an agent's messages from it.

The page reads for the person, too: when the newest message has sat on a visible tab, scrolled to the end of the transcript, for two seconds, the page moves that person's read cursor to it over the same route `saneha read` uses — so `surdy@web` has a position in the transcript like anyone else, and their own marker moves as they watch. Nothing moves it on a tab in the background, on a transcript scrolled back into history, or before that person has joined, and a burst of messages is one call once the last of them has settled.

A `204` is also what a held poll gets when something changed that was not a message, which is a read cursor moving and nothing else: `saneha read` wakes the channel, the poll comes back with nothing new, and the page looks again and moves the rule. Without that a rule would sit where it was until the next message happened to land, up to a minute later. The channel list and the participants are refreshed on the same beat. A tab in the background holds nothing open — the poll in flight is dropped, not merely ignored — and picks up where it left off when it comes back.

`GET /health` reports `held_waits`: how many requests the server is holding open on purpose right now, the per-participant waits and the viewer's polls together.

Tests start a server in-process on a port the OS picks, so nothing needs to be running first:

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
