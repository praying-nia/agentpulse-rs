//! Domain-separated Relay authentication derivation and proofs.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{RelayEndpoint, RelayError, protocol::RouteRegistration};

type HmacSha256 = Hmac<Sha256>;
const ROUTE_DOMAIN: &[u8] = b"agentpulse.relay.v1.route\0";
const CLIENT_AUTH_DOMAIN: &[u8] = b"agentpulse.relay.v1.client-auth\0";
const HOST_PROOF_ROLE: u8 = 1;
const CLIENT_PROOF_ROLE: u8 = 2;

/// Derived per-device routing material safe to transmit only inside Relay TLS.
pub struct RelayRouteCredential {
    route_id: String,
    authentication_key: Zeroizing<[u8; 32]>,
}

impl RelayRouteCredential {
    /// Returns the opaque Base64URL route identifier.
    #[must_use]
    pub fn route_id(&self) -> &str {
        &self.route_id
    }

    /// Returns the HMAC key used only for Relay edge authentication.
    #[must_use]
    pub fn authentication_key(&self) -> &[u8; 32] {
        &self.authentication_key
    }

    /// Converts the credential to an authenticated Host registration entry.
    #[must_use]
    pub fn registration(&self) -> RouteRegistration {
        RouteRegistration {
            route_id: self.route_id.clone(),
            authentication_key: URL_SAFE_NO_PAD.encode(self.authentication_key.as_slice()),
        }
    }
}

impl std::fmt::Debug for RelayRouteCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RelayRouteCredential")
            .field("route_id", &self.route_id)
            .finish_non_exhaustive()
    }
}

/// Hashes one raw device bearer token into the Host-stored device root.
#[must_use]
pub fn device_root_from_token(token: &str) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(Sha256::digest(token.as_bytes()).into())
}

/// Derives a route ID and independent edge-authentication key.
pub fn derive_route(
    device_root: &[u8; 32],
    endpoint: &RelayEndpoint,
) -> Result<RelayRouteCredential, RelayError> {
    let authority = endpoint.authority();
    let route = keyed(device_root, ROUTE_DOMAIN, authority.as_bytes())?;
    let authentication_key = keyed(device_root, CLIENT_AUTH_DOMAIN, authority.as_bytes())?;
    Ok(RelayRouteCredential {
        route_id: URL_SAFE_NO_PAD.encode(route),
        authentication_key: Zeroizing::new(authentication_key),
    })
}

/// Converts a one-time Host enrollment token to its stored proof key.
#[must_use]
pub fn host_authentication_key(token: &str) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(Sha256::digest(token.as_bytes()).into())
}

pub(crate) fn decode_32(field: &'static str, value: &str) -> Result<[u8; 32], RelayError> {
    let bytes = URL_SAFE_NO_PAD.decode(value)?;
    bytes
        .try_into()
        .map_err(|_| RelayError::invalid(field, "must contain exactly 32 bytes"))
}

pub(crate) fn host_proof(
    key: &[u8; 32],
    connection_id: &str,
    nonce: &[u8; 32],
    expires_at_unix_seconds: i64,
    host_id: &str,
    routes: &[RouteRegistration],
) -> Result<String, RelayError> {
    let mut transcript = proof_prefix(
        HOST_PROOF_ROLE,
        connection_id,
        nonce,
        expires_at_unix_seconds,
    )?;
    transcript.extend_from_slice(uuid_bytes("host_id", host_id)?.as_slice());
    let count = u16::try_from(routes.len())
        .map_err(|_| RelayError::invalid("routes", "too many routes"))?;
    transcript.extend_from_slice(&count.to_be_bytes());
    for route in routes {
        transcript.extend_from_slice(&decode_32("route_id", &route.route_id)?);
        transcript.extend_from_slice(&decode_32("authentication_key", &route.authentication_key)?);
    }
    Ok(URL_SAFE_NO_PAD.encode(keyed(key, b"", &transcript)?))
}

pub(crate) fn client_proof(
    key: &[u8; 32],
    connection_id: &str,
    nonce: &[u8; 32],
    expires_at_unix_seconds: i64,
    route_id: &str,
) -> Result<String, RelayError> {
    let mut transcript = proof_prefix(
        CLIENT_PROOF_ROLE,
        connection_id,
        nonce,
        expires_at_unix_seconds,
    )?;
    transcript.extend_from_slice(&decode_32("route_id", route_id)?);
    Ok(URL_SAFE_NO_PAD.encode(keyed(key, b"", &transcript)?))
}

pub(crate) fn verify_proof(expected: &str, supplied: &str) -> bool {
    let Ok(expected) = decode_32("proof", expected) else {
        return false;
    };
    let Ok(supplied) = decode_32("proof", supplied) else {
        return false;
    };
    bool::from(expected.ct_eq(&supplied))
}

fn proof_prefix(
    role: u8,
    connection_id: &str,
    nonce: &[u8; 32],
    expires_at_unix_seconds: i64,
) -> Result<Vec<u8>, RelayError> {
    let mut value = Vec::with_capacity(64);
    value.extend_from_slice(b"agentpulse.relay.v1.proof\0");
    value.push(role);
    value.extend_from_slice(uuid_bytes("connection_id", connection_id)?.as_slice());
    value.extend_from_slice(nonce);
    value.extend_from_slice(&expires_at_unix_seconds.to_be_bytes());
    Ok(value)
}

fn uuid_bytes(field: &'static str, value: &str) -> Result<[u8; 16], RelayError> {
    let uuid =
        Uuid::parse_str(value).map_err(|error| RelayError::invalid(field, error.to_string()))?;
    if uuid.get_version_num() != 7 {
        return Err(RelayError::invalid(field, "must be UUIDv7"));
    }
    Ok(*uuid.as_bytes())
}

fn keyed(key: &[u8], prefix: &[u8], value: &[u8]) -> Result<[u8; 32], RelayError> {
    let mut hmac = HmacSha256::new_from_slice(key)
        .map_err(|_| RelayError::invalid("authentication_key", "HMAC key is invalid"))?;
    hmac.update(prefix);
    hmac.update(value);
    Ok(hmac.finalize().into_bytes().into())
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn derivation_is_stable_and_domain_separated() -> Result<(), Box<dyn Error>> {
        let endpoint = "ap.nonamenona.top:19191".parse::<RelayEndpoint>()?;
        let root = device_root_from_token("device-secret");
        let route = derive_route(&root, &endpoint)?;
        assert_ne!(
            decode_32("route", route.route_id())?,
            *route.authentication_key()
        );
        assert_eq!(route.route_id().len(), 43);
        Ok(())
    }

    #[test]
    fn canonical_cross_language_vector_is_stable() -> Result<(), Box<dyn Error>> {
        let endpoint = "relay.example.com:19191".parse::<RelayEndpoint>()?;
        let root = device_root_from_token("fixture-device-token");
        assert_eq!(
            URL_SAFE_NO_PAD.encode(root.as_slice()),
            "oGfzrDgiYXwMlMpR_Ak4u18jLK62P3GBWMnnpTLeA6k"
        );
        let route = derive_route(&root, &endpoint)?;
        assert_eq!(
            route.route_id(),
            "aCqsldNQU3q4F4wpLIb_VHzyh51lR6SwzzuK9dno5Mk"
        );
        assert_eq!(
            URL_SAFE_NO_PAD.encode(route.authentication_key()),
            "hVBZB_Ak8IDNLGAsJuLi4G_Jhdv1WwnK7YPikP0EGhE"
        );
        let connection_id = "018f10a1-1e20-77d2-9d90-80ab2f45a711";
        let host_id = "018f10a0-fd57-7c08-bb2a-9b61c761a62f";
        let mut nonce = [0_u8; 32];
        for (index, byte) in nonce.iter_mut().enumerate() {
            *byte = u8::try_from(index)?;
        }
        let expires_at = 2_000_000_000;
        let registration = route.registration();
        let host_key = host_authentication_key("fixture-host-enrollment-token");
        assert_eq!(
            host_proof(
                &host_key,
                connection_id,
                &nonce,
                expires_at,
                host_id,
                &[registration]
            )?,
            "ZI8B-3lR_2n6C7kmCoImDlzr-zepmT6Qi3FpaNNBiTc"
        );
        assert_eq!(
            client_proof(
                route.authentication_key(),
                connection_id,
                &nonce,
                expires_at,
                route.route_id()
            )?,
            "5GTj5v2L17ruKW7gkNVZAlwuwo309RR8sYYF2U8FoIk"
        );
        Ok(())
    }
}
