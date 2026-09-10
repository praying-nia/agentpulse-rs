# agentpulse-host

User-facing Windows/Linux/macOS Host for AgentPulse observation and approvals.

For a complete walkthrough, see the [中文详细使用手册](../../docs/USER_GUIDE.zh-CN.md).

Windows uses a current-user Named Pipe for administration and protected ACLs for
configuration and credentials. The Codex App Server and desktop proxy use IPv4
loopback WebSockets; Codex and its descendants run in a kill-on-close Job Object.
Use a dedicated application directory with `--data-dir`, or the default
`ProjectDirs` location. See [platform contracts](../agentpulse-platform/README.md).

```powershell
cargo build -p agentpulse-host
.\target\debug\agentpulse.exe init --name "Windows Host"
.\target\debug\agentpulse.exe serve --discover-threads --bind <YOUR_PRIVATE_IPV4>
# In another terminal, using the same --data-dir if customized:
.\target\debug\agentpulse.exe codex
.\target\debug\agentpulse.exe status
.\target\debug\agentpulse.exe stop
```

The Host resolves the native `codex.exe` from PATH or the official npm package;
PowerShell execution policy does not need to change. For a custom installation,
pass `serve --codex 'C:\path with spaces\codex.exe'`. `agentpulse codex` defaults
to that same executable and connects to the observing proxy reported by Host.
On Windows, it obtains the root-only loopback URI and a separate 256-bit proxy
token through the current-user protected admin channel, then passes the token
only in the Codex child environment with `--remote-auth-token-env`. The token is
not printed in status, logs, command arguments, or errors. A new Host
configuration allocates new proxy credentials; launch through `agentpulse codex`
instead of saving them. The Native phone port remains stable.

Windows local Runtime, Proxy and Host CLI acceptance use Codex 0.153.4. This
version still follows the best-effort compatibility policy against the bundled
0.153.0 schema. Phone pairing, public Relay, and real model Turn acceptance on
Windows remain a separate device test; local smoke tests do not certify them.

## Setup

```bash
cargo install --path agentpulse-host
agentpulse init --name "Studio Host"
agentpulse threads add <CODEX_THREAD_UUIDV7>
printf '%s\n' '<RELAY_ENROLLMENT_TOKEN>' | \
  agentpulse relay configure --endpoint relay.example.com:2333 --token-stdin
agentpulse serve --bind 127.0.0.1
```

`serve` requires a verified Codex CLI version or a valid newer SemVer under the Provider compatibility policy, starts the managed Codex App Server Provider, authenticated Native WSS, mDNS service, private admin socket, and foreground health loop. Omit `--bind` only when the machine has exactly one private/link-local address. Native WSS uses stable port `49320` by default so saved pairing credentials survive Host restarts even when mDNS is unavailable; use `--port` to select another stable port when necessary. In another terminal, launch Codex through the managed server:

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

After terminal approval, `pair` waits for the new device's Native Relay route to
be acknowledged before returning success. The Host registers each device on its
own connector and checks for newly issued credentials every 100 ms, so an old
waiting registration or active tunnel cannot delay a new pairing. If readiness
cannot be confirmed within 30 seconds, pairing returns an error and revokes the
unissued credential. Rebuild and restart the Host to load this behavior.

## Known issue: desktop plan confirmation

On Codex CLI 0.153.0, choosing “实施计划” on Android starts implementation, but
its desktop TUI can leave the local “Implement this plan?” popup open. This is a
Codex TUI lifecycle issue: a remote turn start does not dismiss that local popup.
Dismiss the stale popup with Escape; do not confirm it again. AgentPulse does not
patch Codex for this issue. The phone's action has already started the turn.

## Public direct pairing

For a public Host, configure direct access once. Phone interaction remains scan,
then approve on the Host. Both a public IP on the local interface and a cloud/NAT
public address mapped to a private local interface are supported:

```bash
agentpulse direct configure \
  --bind 192.168.1.20 \
  --native-endpoint host.example.com:44320 \
  --pairing-endpoint host.example.com:44321
agentpulse direct status
# Restart an existing Host before using the new configuration:
agentpulse stop
agentpulse serve --discover-threads
# In another terminal:
agentpulse pair
```

Replace the example addresses with your actual interface and external endpoints.
The default local ports are 49320 (Native) and 49321 (pairing); override with
`--native-port` and `--pairing-port`. For the mapping example, forward public TCP
44320 to local 49320 and public TCP 44321 to local 49321. Without mapping, specify
the same local and external ports. IPv6 external endpoints use `[IPv6]:port`.
A concrete local interface IP is required; wildcard and loopback binds are not
direct configuration inputs. Permit both TCP ports in the applicable firewall.
The pairing port listens only while `pair` runs, for at most the session lifetime
plus bounded transport shutdown. Port occupation is an error, not a reason to
choose another port. Mapping must forward TCP without terminating Host TLS.

Direct configuration takes effect on `serve` startup. Explicit `serve --bind` or
`--port` values must agree with it. The `ap` helper automatically uses configured
direct listening settings; `ap`, `ap qrcode`, and phone scanning remain unchanged.
Run `ap stop` and then `ap` after changing settings. A running Host continues to
use its old effective configuration until restarted; `agentpulse status` shows
its effective direct destinations, while `direct status` shows saved settings.

With direct configured, the QR carries v2 direct discovery automatically and
pairing requires no Relay. Without it, the existing v1 Relay flow is unchanged.
If both are configured, existing Relay connectors stay running but new QR pairing
uses direct. Failure never silently switches routes. Run `agentpulse direct
disable` and restart to restore the legacy pairing choice. Existing phone
profiles do not switch automatically; rescan to update a changed public endpoint.

Upgrade Android before scanning a direct QR. The app saves the public Native
endpoint, displays Direct, and reconnects without LAN discovery overriding it.
TLS fingerprint/CA verification, short-lived QR secrets, terminal approval, and
per-device revocation are retained. See [discovery v2](../../agentpulse-protocol/pairing-v2.md).

Acceptance on a real public deployment must use a Host with no Relay configured
and a phone on mobile data, then check pairing, Native traffic, disconnect /
reconnect and app restart. Confirm connection destinations with network records.
Local TLS tests do not certify external firewall rules, mappings or mobile reachability.
