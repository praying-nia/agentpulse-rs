# agentpulse-relay

Authenticated public tunnel for QR-only first pairing and the read-only
AgentPulse Native path. Relay terminates publicly trusted outer TLS,
authenticates disjoint Host registrations and stable/ephemeral routes, then
pumps opaque inner Host TLS bytes with fixed buffers and deadlines. It does not
receive QR bootstrap/device Tokens, pairing messages, or Session/Event
plaintext, and it stores route registrations only in memory.

The canonical state machine, derivation transcript, limits, and cross-language
fixtures live in the separate `agentpulse-protocol` specification repository as
Relay v1. Server hardening, CI deployment, rollback, and certificate rotation are
documented in [`../deploy`](../deploy).
