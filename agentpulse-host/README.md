# agentpulse-host

User-facing Linux/macOS Host for AgentPulse observation and approvals.

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

For a one-command Linux workflow, install the workspace release binary and
[`scripts/ap`](../scripts/ap) on `PATH`. The helper starts or reuses the Host,
keeps the Host and terminal UI on the same Codex profile, and uses the systemd
user manager to leave the Host running when the terminal UI exits:

```bash
ap             # codex
ap nona        # codex-nona
ap rinia       # codex-rinia
ap qrcode      # show another one-time pairing QR
ap status
ap stop
```

Starting or switching the Host shows one pairing QR before opening Codex; press
`Ctrl+C` to skip it and run `ap` again, leaving the Host in the background.
`ap qrcode` opens another one-time pairing session whenever the Host is already
running. Each foreground `ap` invocation opens Codex in the shell's current
directory even when it reuses an older background Host. Additional arguments
are forwarded to Codex; an explicit `-C` or `--cd` takes precedence, for example
`ap rinia -C /path/to/project`.

The background Host inherits the current shell's upper- and lower-case HTTP,
HTTPS, ALL, and NO proxy variables. If they change, the next `ap` invocation
restarts the Host with the new proxy environment.

The shortcut does not read, change, or persist the manual `agentpulse threads`
allowlist. It follows only threads started or resumed through its managed App
Server while that Host is running; stopping the Host discards that runtime
mapping. The Host identity, paired-device credentials, and Relay configuration
remain machine-level so a stopped shortcut does not force the phone to pair
again.

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
