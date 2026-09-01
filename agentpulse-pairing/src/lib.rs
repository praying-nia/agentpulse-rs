//! Secure proximity pairing and persistent per-device credentials.
//!
//! Pairing is deliberately separate from the long-lived Native protocol. A
//! short-lived, certificate-pinned WSS endpoint issues a device-specific token;
//! the Native listener later validates that token without storing it in clear.

mod error;
mod protocol;
mod qr;
mod server;
mod store;

pub use error::PairingError;
pub use protocol::{
    PAIRING_PROTOCOL_VERSION, PAIRING_WEBSOCKET_PATH, PAIRING_WEBSOCKET_SUBPROTOCOL, PairingBundle,
    PairingErrorCode, PairingRequest, PairingServerMessage, decode_pairing_request,
    decode_pairing_uri, decode_server_message, encode_pairing_request, encode_server_message,
};
pub use qr::terminal_qr;
pub use server::{PairingOutcome, PairingSession};
pub use store::{
    DeviceCredentialDigest, DeviceCredentialSummary, FileCredentialAuthorizer, HostCredentialStore,
    HostIdentitySnapshot,
};

/// Fixed BLE GATT service for AgentPulse Pairing v1.
pub const PAIRING_BLE_SERVICE_UUID: &str = "d22e50f9-015e-53ba-be49-3e4d235f3288";

/// Secure long-read characteristic containing the pairing URI.
pub const PAIRING_BLE_BUNDLE_CHARACTERISTIC_UUID: &str = "ea63bfc9-87c3-5074-aa37-49b6a617569b";
