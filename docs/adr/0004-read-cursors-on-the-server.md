---
status: accepted
date: 2026-09-04
---
# Read cursors live on the server, per participant per channel

A stateless design, where each client remembers the last message id it saw, is simpler and is what ntfy-style tools do. We keep the cursor on the server instead because two things need to know what a given participant has not yet read: the relay deciding whether to wake it, and the viewer showing a person where each agent is in the conversation. Only `read` advances a cursor; `wait` and history reads never do, so observing a channel is always side-effect free.
