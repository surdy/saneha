---
status: accepted
date: 2026-09-04
---
# saneha runs its own server instead of wrapping ntfy

ntfy was the obvious zero-code transport: on-demand topics, no accounts, a web UI, and history. We rejected it because the things saneha needs next are message-level concepts, not delivery concepts: recipients, per-participant read cursors, participants with identities, and filtered wait streams. ntfy models none of those, and encoding them into titles and tags would have boxed us in within weeks. A single Rust binary with SQLite costs a few hundred lines and owns its schema.

## Considered options

- **ntfy behind the CLI.** Zero server code. Rejected for the reasons above; the CLI would have hidden it, but every feature past plain text would have been a workaround.
- **Matrix, Mattermost, Zulip.** All require accounts and pre-registration, which violates the no-registration requirement.
- **agent-bus, cross-agent-teams-mcp.** Purpose-built for agent chat but localhost-only.
- **MCP Agent Mail.** Cross-machine over HTTP, but built around agents sharing one repo with file reservations. Far more machinery than an impromptu channel.
