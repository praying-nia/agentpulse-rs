# agentpulse-relay

Optional authenticated public tunnel for the read-only AgentPulse Native path.
Relay terminates publicly trusted outer TLS, authenticates one Host and paired
device route, then pumps opaque inner Host-CA TLS bytes with fixed buffers and
deadlines. It does not receive Native bearer Tokens or Session/Event plaintext,
and it stores route registrations only in memory.

The canonical state machine, derivation transcript, limits, and cross-language
fixtures live in the separate `agentpulse-protocol` specification repository as
Relay v1. Server hardening, CI deployment, rollback, and certificate rotation are
documented in [`../deploy`](../deploy).
