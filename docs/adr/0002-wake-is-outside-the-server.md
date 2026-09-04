---
status: accepted
date: 2026-09-04
---
# Waking idle participants is not the server's job, and Madari is an optional adapter

The obvious design was to build saneha on top of Madari, the terminal that already knows each pane's agent state and can type into it. We decided instead that the server only holds wait streams open and knows nothing about harnesses or terminals. Waking is done by clients, on a ladder: a participant's own background wait (which on Claude Code re-invokes the model when it exits), then a per-host relay that nudges idle panes through Madari's control CLI, then a person. saneha never links against Madari and Madari never depends on saneha; the relay shells out to the `madari` CLI, and the unattended send it needs is a separate decision in the Madari repository.

## Consequences

- Any harness in any terminal can send and read. Wake quality degrades by rung; it never blocks participation.
- Harness-specific behaviour lives in the skill text and the relay, never in the server or the message schema.
- A relay for a different terminal, or an MCP push channel for one harness, is an additional rung, not a redesign.
