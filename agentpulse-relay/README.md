# agentpulse-relay

Authenticated public tunnel for QR-only first pairing and the approval-capable
AgentPulse Native path. Relay terminates publicly trusted outer TLS,
authenticates disjoint Host registrations and stable/ephemeral routes, then
pumps opaque inner Host TLS bytes with fixed buffers and deadlines. It does not
receive QR bootstrap/device Tokens, pairing messages, or Session/Event
plaintext, and it stores route registrations only in memory.

The canonical state machine, derivation transcript, limits, and cross-language
fixtures live in the separate `agentpulse-protocol` specification repository as
Relay v1. Server hardening, CI deployment, rollback, and certificate rotation are
documented in [`../deploy`](../deploy).

The per-device Host connector requires capacity for all 16 paired-device waiting
registrations plus one QR bootstrap registration. Relay therefore permits 17
waiting registrations and 68 outer connections, including tunnel peers and
bounded reconnect overlap. Deploy this Relay alongside per-device Host routing;
the older four-registration limit can prevent new QR pairing from starting.
