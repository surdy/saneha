---
status: accepted
date: 2026-09-09
---
# A version is what is deployed

`CARGO_PKG_VERSION` was `0.1.0` from the first commit to the twelfth deploy. `/health` reported it, nothing read it, and the README said so out loud — "a version nobody bumps is a check that never fires". Meanwhile the thing people actually needed a name for, *which build is running on quadhost*, was a short commit SHA in a Quadlet unit: exact, immutable, and unreadable. `sha-d611048` says nothing about what changed or whether you want it.

So a release is cut when something is deployed, and the version names that. Tagging is part of the deploy, in the same pull request that bumps `Image=`; the notes are what that PR already had to say — what changed, whether a migration is involved, whether mixed versions are safe — published rather than left in a merge commit. The alternative, releasing when there is "enough" to tell, decouples the released version from the deployed one and puts `/health`'s `version` back to answering nothing.

Numbers are semantic versioning starting at 0.2.0. The `0.` is meant: the wire and the CLI may still change without a major bump, and every deploy states its own compatibility rather than leaning on the number to imply it. [v1-scope.md](../v1-scope.md)'s list is in fact all built, which is an argument for calling this 1.0.0 — it was considered and declined, because 1.0 is a promise to people who are not here yet, and the version's job right now is to name a build rather than to make a commitment.

The image workflow publishes a `v*` tag under its version as well as its SHA, so a release is pullable by the number its notes are written against. **The Quadlet unit goes on pinning the SHA.** A version tag is a name somebody could move, and the runbook's drift check — the repository copy is the record of what is deployed — is only worth something if what it names cannot have moved underneath it.

The notes live in [CHANGELOG.md](../../CHANGELOG.md) rather than only in a GitHub release, for the reason this repository keeps everything else in it: the server may be reachable on a LAN with no route out, and a record you need a network and an account to read is not one the person at the terminal has.

## Consequences

- **`/health`'s `version` becomes true.** It answers "what is this server", which is the one job it could usefully have and has never had. The skill digest beside it stays the check that matters, because the two answer different questions: a build from `main` between releases carries the older version and its own skill, and it is the skill an agent is following.
- **The pointer stops carrying a version, and every machine re-inits once.** `saneha init` wrote `saneha-managed: <version>` into the file it installs, and the staleness check compares the whole file — so the first bump would have made every pointer stale and asked every machine to run `init` again, on every release, which is the per-change chore [ADR-0006](0006-the-installed-skill-is-a-pointer.md) exists to end. The marker now says only that the file is saneha's. Any value still counts as saneha's, so the pointers written before 0.2.0 are replaced rather than orphaned — once, and then never again by a release.
- A deploy is now three things rather than two: bump `Image=`, write the notes, tag. The first two were already one pull request and the third is a command, so the cost is the notes, which the deploy PR was writing anyway.
- **Nothing enforces that the tag and the deployed image agree.** They agree because the same pull request carries both. A check could compare `/health`'s version against the newest tag, and is not worth building until the two have disagreed once.
