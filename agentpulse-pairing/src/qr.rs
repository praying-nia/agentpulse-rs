//! Terminal QR rendering.

use qrcode::{QrCode, render::unicode};

use crate::PairingError;

/// Renders one pairing URI as a dense Unicode QR code.
pub fn terminal_qr(uri: &str) -> Result<String, PairingError> {
    let code = QrCode::new(uri.as_bytes()).map_err(|error| PairingError::InvalidField {
        field: "pairing_uri",
        reason: error.to_string(),
    })?;
    Ok(code
        .render::<unicode::Dense1x2>()
        .quiet_zone(true)
        .dark_color(unicode::Dense1x2::Dark)
        .light_color(unicode::Dense1x2::Light)
        .build())
}
