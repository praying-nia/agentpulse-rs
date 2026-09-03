# agentpulse-channel-native

Native Session/Event synchronization, interaction, and command Channel for AgentPulse.

The crate pairs a Bridge-facing `NativeChannelPort` with a RuntimeHost-owned `NativeChannelSource`. The Source serves one explicitly handshaken client over loopback WebSocket or authenticated private-LAN WSS, while the Port queues subscribed Session/Event deliveries. `NativeChannelHandle` exposes the actual ephemeral address, lifecycle health, active client ID, counters, and the last diagnostic.

```rust,no_run
use agentpulse_bridge::RuntimeHost;
use agentpulse_channel_native::{NativeChannel, NativeChannelConfig};
use agentpulse_core::ChannelId;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let parts = NativeChannel::build(NativeChannelConfig::new(ChannelId::new()))?;
let (port, source, handle) = parts.into_parts();

let mut host = RuntimeHost::new();
host.register_channel(port, source)?;
host.start()?;

let address = handle.snapshot().local_address;
// Give `address` to the local native client. It must connect with the fixed
// path/subprotocol, then perform Hello → Discover → Subscribe.

host.stop()?;
# Ok(())
# }
```

The Channel declares exactly `NOTIFICATION | SESSION_VIEW | APPROVAL | TEXT_INPUT | FORM_INPUT | REALTIME_SYNC | REMOTE_COMMAND`. It retains and replays the current Host run through 128-Event pages, then atomically sends the current Session and pending interactions before entering ordered live delivery. `submit_interaction_response` accepts only a correlated approval/form response from an actively subscribed Session, and `submit_command` accepts only a typed command on the same route. Disconnect and Source-generation shutdown remove connection-owned subscriptions without deleting the Host run history.

`NativeChannelConfig::new` retains the `127.0.0.1:0` compatibility boundary. `NativeChannelConfig::authenticated_lan` requires an explicit private/link-local address, TLS identity, and live bearer authorizer. The authenticated upgrade client ID must equal the following Client Hello ID, and revocation is observed without restarting the Channel. Both modes use path `/agentpulse/native/v3`, subprotocol `agentpulse.native.v3`, 1 MiB messages, a 256-frame output queue, 15-second Ping, and 45-second idle timeout by default.

The authoritative wire and lifecycle contract is [Native Transport v3](../../../agentpulse-protocol/native-transport-v3.md). This crate mirrors its cross-language fixtures under `tests/fixtures/native-v3` and verifies semantic round trips plus byte-identical fixture mirroring in an umbrella checkout.
