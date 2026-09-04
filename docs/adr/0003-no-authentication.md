---
status: accepted
date: 2026-09-04
---
# No authentication; the network boundary is the trust boundary

saneha has no accounts, tokens, or identity verification. Anyone who can reach the server can join any channel as any identity. This is deliberate: the server is reachable only from the home LAN and the tailnet (split DNS on clusterfault.com with the Cloudflare proxy off), everyone on that network is the owner, and the requirement was that no agent be registered or authorised in advance. Adding a shared secret later is a one-line header check that does not change the model; adding real per-agent identity would, and we chose not to pay for it.
