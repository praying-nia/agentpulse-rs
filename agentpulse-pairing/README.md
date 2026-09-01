# agentpulse-pairing

Secure QR bootstrap and credential lifecycle for AgentPulse.

`HostCredentialStore` atomically owns one stable Host UUIDv7, Provider/Channel IDs, an app-scoped CA, a renewable 90-day leaf certificate, explicit Codex thread IDs, and at most 16 device credential hashes. Directories and files are restricted to `0700`/`0600` on Unix. A malformed or unavailable store fails authorization closed.

`PairingSession` opens one loopback WSS endpoint for two minutes. The Host exposes it only through an authenticated public Relay route derived from the random bootstrap Token in a terminal QR code. Android pins the QR leaf fingerprint inside that opaque tunnel; USB, ADB, Bluetooth, shared LAN, deep links, and manual URI entry are excluded. The session allows at most five requests, requires local approval, and issues one random bearer Token per Android installation. `FileCredentialAuthorizer` reloads state on every check, so revocation also invalidates an active Native connection.

The canonical contract and cross-language fixtures are in [Pairing v1](../../agentpulse-protocol/pairing-v1.md). This crate mirrors those fixtures and checks byte equality in the umbrella checkout.
