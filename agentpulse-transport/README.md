# agentpulse-transport

Bounded transport primitives used by concrete AgentPulse adapters.

The current implementation provides a synchronous loopback-only WebSocket server with exact path and subprotocol validation, text-only application messages, configurable handshake/I/O deadlines, complete-message limits, control-frame handling, and bounded close behavior. It deliberately contains no AgentPulse domain or Native control semantics.

`LoopbackWebSocketConfig` rejects non-loopback binds and invalid limits. `LoopbackWebSocketListener::try_accept` supports a stoppable polling worker; each accepted `LoopbackWebSocket` exposes complete text/control outcomes and enforces outbound message size.

LAN binding, TLS, authentication, Relay tunneling, reconnect policy, queues, and persistence do not belong to this crate's current scope.
