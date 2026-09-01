# agentpulse-host

User-facing Linux/macOS Host for the read-only AgentPulse product path.

## Setup

```bash
cargo install --path agentpulse-host
agentpulse init --name "Studio Host"
agentpulse threads add <CODEX_THREAD_UUIDV7>
printf '%s\n' '<RELAY_ENROLLMENT_TOKEN>' | \
  agentpulse relay configure --endpoint relay.example.com:2333 --token-stdin
agentpulse serve --bind 127.0.0.1
```

`serve` requires the exact supported Codex CLI version, starts the managed Codex App Server Provider, authenticated Native WSS, mDNS service, private admin socket, and foreground health loop. Omit `--bind` only when the machine has exactly one private/link-local address. Native WSS uses stable port `49320` by default so saved pairing credentials survive Host restarts even when mDNS is unavailable; use `--port` to select another stable port when necessary. In another terminal, launch Codex through the managed server:

```bash
agentpulse codex -- <additional-codex-arguments>
```

## Pair and operate

With the Relay-configured Host running, publish a two-minute QR-only pairing route:

```bash
agentpulse pair
```

`pair` waits until the ephemeral route is authenticated and publicly available, then prints exactly one QR code. Android needs only Internet access and the camera: USB, ADB, Bluetooth, a shared LAN, deep links, and manual URI entry are not pairing paths. Credential issuance still requires explicit terminal approval.

```bash
agentpulse status
agentpulse devices list
agentpulse devices revoke <ANDROID_CLIENT_UUIDV7>
agentpulse stop
agentpulse credentials rotate --confirm-revoke-all
```

Rotation is allowed only while stopped and revokes every device. Host identity and credentials are private local configuration; Session and Event state is in memory and is not persisted.
