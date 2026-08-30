# agentpulse-pairing

Secure local bootstrap and credential lifecycle for AgentPulse.

`HostCredentialStore` atomically owns one stable Host UUIDv7, Provider/Channel IDs, an app-scoped CA, a renewable 90-day leaf certificate, explicit Codex thread IDs, and at most 16 device credential hashes. Directories and files are restricted to `0700`/`0600` on Unix. A malformed or unavailable store fails authorization closed.

`PairingSession` opens one private-LAN WSS endpoint for two minutes. Its bootstrap URI is carried by Linux secure BLE GATT or a terminal QR code, pins the current leaf certificate, allows at most five requests, requires local approval, and issues one random bearer token per Android installation. `FileCredentialAuthorizer` reloads state on every check, so revocation also invalidates an active Native connection.

The canonical contract and cross-language fixtures are in [Pairing v1](../../agentpulse-protocol/pairing-v1.md). This crate mirrors those fixtures and checks byte equality in the umbrella checkout.
