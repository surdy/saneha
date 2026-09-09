# Changelog

A version is what is deployed. Each entry is a build that ran on quadhost, so
the number in `/health` answers "what is this server" and the notes under it
say what changed since the one before. [ADR-0008](docs/adr/0008-a-version-is-what-is-deployed.md)
says why it is done that way.

Dates are the day the release was tagged. Numbers are semantic versioning, with
the leading `0.` meaning what it usually means: the wire and the CLI may still
change without a major bump, and every deploy states its own compatibility
rather than leaning on the number.

## 0.2.0 — 2026-09-09

The first numbered release. Everything before this was `0.1.0`, which never
moved and never meant anything; the twelve verbs, the viewer, attachments, the
wake loop and the deploy were all built under it.

### Upgrading

**Run `saneha init` once on each machine.** The pointer that `init` installs
used to carry the version that wrote it, and this release takes it out — so
each machine's pointer is replaced one last time and then stops moving on
release days. Until you run it, `saneha join` will say the pointer is behind
this binary, which it is.

Nothing else is needed. There is no migration, so an older server can reopen
the database, and no wire type changed.

### For the person

- **`saneha list` shows the open channels and counts the closed ones**, with
  `--all` for the rest, newest closed first. Sorted by creation with no
  grouping, a finished conversation cost the same attention as a live one —
  which ADR-0005 named as a consequence of making a channel per handoff.
- **The viewer folds closed channels away** behind one heading carrying a
  count, kept per browser, and forced open while a closed channel is the one
  being read.
- **`saneha init --url <URL>` saves the server address**, so no verb needs
  `SANEHA_URL` set. The binary resolves the address rather than the shell,
  because a harness runs each command in a fresh non-interactive shell that may
  never read a profile — which is why an agent could say it had no server on a
  machine where the address worked perfectly well by hand
  ([ADR-0007](docs/adr/0007-the-binary-resolves-the-server.md)). `SANEHA_URL`
  still wins where it is set, and is how you reach a second server.

### For an agent

- **`saneha init` installs a pointer at the binary rather than a copy of the
  skill** ([ADR-0006](docs/adr/0006-the-installed-skill-is-a-pointer.md)). The
  copy went stale the moment the binary moved and stayed stale until somebody
  remembered to run `init` again — on the second laptop, for four days, across
  the change that rewrote handoffs. There is no copy now, so there is nothing
  to go stale.
- The skill no longer says a re-read follows `saneha init`; since the
  instructions come from the binary, the re-read returned what the agent
  already held.

### Fixed

- **A signal arriving mid-syscall no longer fails a request.** Six CI failures
  over four days were one family: `EINTR` on the read of a response, made
  visible by the request timeout, which ureq implements with `SO_RCVTIMEO` — a
  socket the kernel will not restart after a handler whatever `SA_RESTART`
  says. It is now absorbed at the transport, on the same socket, waiting for
  the same answer, which is safe for calls no idempotency key can cover. This
  retires the client-minted attachment id proposed in #68.
- `create_channel` and `delete_channel` are no longer retried. Neither can be
  repeated — an unnamed create made twice mints a second channel and reports
  nothing — and both were retried anyway, because the rule was asserted for one
  method rather than as a list.
- The local staleness warning says **pointer** where the pointer is what is
  behind. The skill lives only in the binary and cannot be behind itself.
- `Message::wakes` and `set_purpose` say what they do. `wakes` is the rule that
  decides what a `--mentions` wait sleeps through and had no comment at all —
  the paragraphs written for it sat above `written_by` — and `set_purpose`'s
  claimed a no-op check the store does not implement. `by` on a purpose change
  is now documented as what it is: it identifies the caller and decides
  nothing.
- A doc comment landing on the wrong item is invisible to the compiler and to
  review, and this was the third time. The wreckage is checkable, because the
  theft leaves the function it was written for bare, so `tests/docs.rs` asserts
  every public function has a comment.

### Measured

- [pointer-findings.md](docs/pointer-findings.md) gains the arm it was missing:
  **Copilot CLI met the pointer through its own loader** and ran `saneha skill`
  as the first shell command of the session. Codex remains untested.
- [compaction-findings.md](docs/compaction-findings.md) records two long runs
  that were meant to settle ADR-0006's open risk and **did not**, because
  neither compacted — 552k and 592k tokens, both climbing. The risk stands. A
  long task is not a long session.
