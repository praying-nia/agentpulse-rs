# agentpulse-host

User-facing Linux/macOS Host for the local read-only AgentPulse product path.

## Setup

Linux builds require the D-Bus development headers used by the BlueZ pairing backend. On Ubuntu:

```bash
sudo apt-get install libdbus-1-dev pkg-config
```

```bash
cargo install --path agentpulse-host
agentpulse init --name "Studio Host"
agentpulse threads add <CODEX_THREAD_UUIDV7>
agentpulse serve --bind 192.168.1.20
```

`serve` requires the exact supported Codex CLI version, starts the managed Codex App Server Provider, authenticated Native WSS, mDNS service, private admin socket, and foreground health loop. Omit `--bind` only when the machine has exactly one private/link-local address. Native WSS uses stable port `49320` by default so saved pairing credentials survive Host restarts even when mDNS is unavailable; use `--port` to select another stable port when necessary. In another terminal, launch Codex through the managed server:

```bash
agentpulse codex -- <additional-codex-arguments>
```

## Pair and operate

With the Host running, open a two-minute pairing session:

```bash
agentpulse pair
```

Linux attempts secure BLE nearby pairing and always prints a QR/manual fallback. macOS uses the QR path. Both require explicit terminal approval before issuing credentials.

```bash
agentpulse status
agentpulse devices list
agentpulse devices revoke <ANDROID_CLIENT_UUIDV7>
agentpulse stop
agentpulse credentials rotate --confirm-revoke-all
```

Rotation is allowed only while stopped and revokes every device. Host identity and credentials are private local configuration; Session and Event state is in memory and is not persisted.
