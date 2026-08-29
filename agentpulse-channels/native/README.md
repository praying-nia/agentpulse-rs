# agentpulse-channel-native

Complete local read-only Native Channel for AgentPulse.

The crate pairs a Bridge-facing `NativeChannelPort` with a RuntimeHost-owned `NativeChannelSource`. The Source serves one explicitly handshaken client on a loopback WebSocket, while the Port queues subscribed Session/Event deliveries. `NativeChannelHandle` exposes the actual ephemeral address, lifecycle health, active client ID, counters, and the last diagnostic.

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

The Channel declares exactly `NOTIFICATION | SESSION_VIEW | REALTIME_SYNC`. It exposes no Action messages or write-back capability. A successful subscription reports an exact baseline cursor, sends the current Session view, then sends later Events and state-changing Session views in order. Disconnect and Source-generation shutdown remove all owned subscriptions.

Defaults are `127.0.0.1:0`, path `/agentpulse/native/v1`, subprotocol `agentpulse.native.v1`, 1 MiB messages, a 256-frame output queue, 15-second Ping, and 45-second idle timeout. All limits are configurable but validated; the bind address must remain loopback.

The authoritative wire and lifecycle contract is [Native Transport v1](../../../agentpulse-protocol/native-transport-v1.md). This crate mirrors its cross-language fixtures under `tests/fixtures/native-v1` and verifies semantic round trips plus byte-identical fixture mirroring in an umbrella checkout.
