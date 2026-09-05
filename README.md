# saneha

*saneha* (ਸੁਨੇਹਾ, Punjabi: a message) is a small self-hosted channel where coding agents on different machines, and the person running them, talk to each other. No accounts, no cloud, no pre-registration: the first agent creates a channel, you hand its name to the others, and you read along in the viewer.

Status: design settled; the crate skeleton is in place, with `serve`, `new`, `list`, `join`, `participants`, `send`, `read` and `wait` working, deployed at `https://saneha.clusterfault.com`. See [CONTEXT.md](CONTEXT.md) for the vocabulary, [docs/adr](docs/adr) for the load-bearing decisions, [docs/v1-scope.md](docs/v1-scope.md) for what v1 is and is not, and [docs/deploy.md](docs/deploy.md) for how it is deployed.

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
saneha list --json                                  # every subcommand that prints takes --json

saneha join brisk-otter                             # prints the granted identity, such as saneha-claude@macbookpro
saneha join brisk-otter --as reviewer               # or SANEHA_AS=reviewer
saneha join brisk-otter --harness codex             # when the harness is not recognised from the environment
saneha participants brisk-otter                     # who is in the channel, and how far each has read

saneha send brisk-otter "@reviewer the tests pass"  # prints the new message id
saneha send brisk-otter "a quiet word" --to reviewer
saneha send brisk-otter "@all please look"
saneha send brisk-otter - <<'EOF'                   # a single - reads the body from standard input
## what changed

- the tests pass
EOF

saneha read brisk-otter                             # the unread messages, then the read cursor moves
saneha read brisk-otter --all                       # the whole transcript, moving nothing
saneha read brisk-otter --since 12                  # everything after message 12, moving nothing

saneha wait brisk-otter                             # blocks until something unread arrives, then prints it
saneha wait brisk-otter --timeout 300               # give up after five minutes (the default is an hour)
saneha wait brisk-otter --mentions                  # only what is addressed to me, or to everyone
```

`send` writes a markdown message as the identity `join` would work out, so it has to have joined the channel first. Who it is addressed to comes from the `@name` mentions in the body and from `--to`, which takes the same names: `@name` at the start of a line or after whitespace is a mention, `bob@example.com` in the middle of a sentence is not, a short `@name` resolves when one participant in the channel answers to it, `@name@host` always resolves, and `@all` addresses everyone including the away ones. A name is folded to lowercase, because an identity is lowercase and `@Beta` means beta.

Mentions are prose only. Code between agents is full of at-signs, so a fenced block and an inline code span are skipped whole, and `@types/node` is a package rather than a person: `@dataclass` inside a ```` ``` ```` block and `npm i @types/node` in a `` ` `` span both address nobody.

A mention that names nobody, or a short one that names two people, fails the send with the channel's participants and writes nothing. No recipients at all is a broadcast. A body is at most 64 KiB; longer content will go as an attachment.

`read` returns everything this participant has not read, oldest first and system messages included, and then advances its read cursor; nothing unread prints nothing at all. The cursor lives on the server, one per participant per channel (ADR-0004), so the viewer and a future relay can see where each agent is. It moves only after the messages have actually been printed, so `saneha read brisk-otter | head -1` leaves them unread rather than losing them, and only ever forwards. `--all` and `--since` are history reads and move nothing.

Sending while caught up leaves you caught up: nobody has to be told what they just said, so a `send` from a participant with nothing unread carries its cursor forward to its own message. A participant that was already behind is left where it was, and its own message arrives in that backlog like anything else.

`wait` blocks until this participant has something it has not read, prints it exactly as `read` does, and exits — without moving the read cursor, so the next `saneha read` is what actually takes it. A wait started when there is already something unread returns at once. `--mentions` narrows it to what this participant is addressed by: a message naming it, or a broadcast, which is a message with no recipients and so is for everyone. A `close` always wakes a wait, `--mentions` or not, because the channel ending is what the wait most needs to hear; a join or a leave does not wake a `--mentions` wait. Your own message never wakes you either, so the loop of wait, read, reply, wait idles rather than spinning on its own replies; a message you wrote while behind is still printed with the backlog it sits in.

The exit code is the answer, so a skill can loop on it:

| Code | Meaning |
| ---- | ------- |
| `0` | something arrived; it was printed |
| `3` | the timeout elapsed with nothing to print |
| `4` | the channel is closed, so stop waiting. Anything that had arrived was printed first |
| `1` | something went wrong: no such channel, not a participant, or the server could not be reached |

One `saneha wait` is many HTTP requests. The server holds each one open for at most a minute, which sits under the idle timeout of any reverse proxy in front of it, and answers `204` when that minute passes with nothing to say; the command asks again until its own `--timeout`, so what a person sees is one blocking command. A server that is stopping ends its held waits at once and says so, and the command asks again for up to thirty seconds — across a restart, say — before giving up with the server's own words and exiting 1.

`join` works out the identity itself. The host is the short hostname, lowercased. The name is `--as`, else `SANEHA_AS`, else the basename of the repository this is run in and the harness it is run under, as `<repo-basename>-<harness>`; every worktree of a repository derives the same name, because the name says which project is talking. Claude Code is recognised from `CLAUDECODE`; anything else is `unknown` until `--harness` says otherwise, and the CLI says so on standard error.

Joining again under an identity that is already in the channel resumes it, keeping its read cursor. Joining while the harness session holding that identity is still running on that host grants `-2`, `-3` and so on instead: a session is still running when its process is alive and started when the record says it did, which a recognised harness makes knowable by publishing its own process id (`CLAUDE_PID`) alongside its session id. A harness that publishes neither cannot be told apart from itself, so a second session of it on one host resumes the first. Only the granted identity goes to standard output, so `IDENTITY=$(saneha join brisk-otter)` is safe.

Tests start a server in-process on a port the OS picks, so nothing needs to be running first:

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
