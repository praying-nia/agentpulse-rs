# agentpulse-channel-native

Native Session/Event synchronization and approval Channel for AgentPulse.

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

The Channel declares exactly `NOTIFICATION | SESSION_VIEW | APPROVAL | REALTIME_SYNC`. A successful subscription reports an exact baseline cursor and pending-interaction count, sends the current Session plus every pending approval, then sends later Events and state-changing Session views in order. `submit_interaction_response` accepts only a correlated opaque option from an actively subscribed Session. Disconnect and Source-generation shutdown remove all owned subscriptions.

`NativeChannelConfig::new` retains the `127.0.0.1:0` compatibility boundary. `NativeChannelConfig::authenticated_lan` requires an explicit private/link-local address, TLS identity, and live bearer authorizer. The authenticated upgrade client ID must equal the following Client Hello ID, and revocation is observed without restarting the Channel. Both modes use path `/agentpulse/native/v1`, subprotocol `agentpulse.native.v1`, 1 MiB messages, a 256-frame output queue, 15-second Ping, and 45-second idle timeout by default.

The authoritative wire and lifecycle contract is [Native Transport v1](../../../agentpulse-protocol/native-transport-v1.md). This crate mirrors its cross-language fixtures under `tests/fixtures/native-v1` and verifies semantic round trips plus byte-identical fixture mirroring in an umbrella checkout.
