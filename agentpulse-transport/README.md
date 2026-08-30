# agentpulse-transport

Bounded transport primitives used by concrete AgentPulse adapters.

The current implementation provides synchronous bounded WebSocket servers for two explicit boundaries: OS-isolated loopback and TLS on a selected private/link-local LAN address. Both enforce an exact path and subprotocol, text-only application messages, configurable handshake/I/O deadlines, complete-message limits, control-frame handling, and bounded close behavior. It deliberately contains no AgentPulse domain or Native control semantics.

`LoopbackWebSocketConfig` rejects non-loopback binds. `TlsWebSocketConfig` rejects wildcard, public, and loopback binds, requires a DER TLS identity, and optionally applies a `BearerTokenAuthorizer` at upgrade time and throughout the live connection. The Native Channel always enables that authorizer; anonymous TLS is reserved for the short-lived fingerprint-pinned pairing endpoint.

Relay tunneling, reconnect policy, application queues, protocol semantics, and persistence do not belong to this crate's scope.
