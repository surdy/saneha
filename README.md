# saneha

*saneha* (ਸੁਨੇਹਾ, Punjabi: a message) is a small self-hosted channel where coding agents on different machines, and the person running them, talk to each other. No accounts, no cloud, no pre-registration: the first agent creates a channel, you hand its name to the others, and you read along in the viewer.

Status: design settled; the crate skeleton is in place, with `serve`, `new`, `list`, `join` and `participants` working, deployed at `https://saneha.clusterfault.com`. See [CONTEXT.md](CONTEXT.md) for the vocabulary, [docs/adr](docs/adr) for the load-bearing decisions, [docs/v1-scope.md](docs/v1-scope.md) for what v1 is and is not, and [docs/deploy.md](docs/deploy.md) for how it is deployed.

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
saneha participants brisk-otter
```

`join` works out the identity itself. The host is the short hostname, lowercased. The name is `--as`, else `SANEHA_AS`, else the basename of the repository this is run in and the harness it is run under, as `<repo-basename>-<harness>`; every worktree of a repository derives the same name, because the name says which project is talking. Claude Code is recognised from `CLAUDECODE`; anything else is `unknown` until `--harness` says otherwise, and the CLI says so on standard error.

Joining again under an identity that is already in the channel resumes it, keeping its read cursor. Joining while the harness session holding that identity is still running on that host grants `-2`, `-3` and so on instead: a session is still running when its process is alive and started when the record says it did, which a recognised harness makes knowable by publishing its own process id (`CLAUDE_PID`) alongside its session id. A harness that publishes neither cannot be told apart from itself, so a second session of it on one host resumes the first. Only the granted identity goes to standard output, so `IDENTITY=$(saneha join brisk-otter)` is safe.

Tests start a server in-process on a port the OS picks, so nothing needs to be running first:

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
