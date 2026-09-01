//! Canonical public Relay endpoint.

use std::{fmt, net::IpAddr, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::RelayError;

/// One public DNS endpoint used consistently by the Host and Android client.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayEndpoint {
    host: String,
    port: u16,
}

impl RelayEndpoint {
    /// Creates and validates a DNS-name endpoint.
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, RelayError> {
        let host = host.into().to_ascii_lowercase();
        validate_host(&host)?;
        if port == 0 {
            return Err(RelayError::invalid("port", "must be non-zero"));
        }
        Ok(Self { host, port })
    }

    /// Returns the canonical lowercase DNS name.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the public TCP port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Returns the exact KDF and display authority.
    #[must_use]
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

impl fmt::Display for RelayEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.authority())
    }
}

impl FromStr for RelayEndpoint {
    type Err = RelayError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.contains("//") || value.contains(['/', '?', '#', '@']) {
            return Err(RelayError::invalid(
                "endpoint",
                "must be a DNS name and port without a scheme or path",
            ));
        }
        let (host, port) = value
            .rsplit_once(':')
            .ok_or_else(|| RelayError::invalid("endpoint", "must use host:port"))?;
        let port = port
            .parse::<u16>()
            .map_err(|_| RelayError::invalid("endpoint", "port is invalid"))?;
        Self::new(host, port)
    }
}

fn validate_host(host: &str) -> Result<(), RelayError> {
    if host.is_empty()
        || host.len() > 253
        || host.ends_with('.')
        || IpAddr::from_str(host).is_ok()
        || !host.is_ascii()
    {
        return Err(RelayError::invalid(
            "host",
            "must be a canonical ASCII DNS name",
        ));
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(RelayError::invalid("host", "contains an invalid DNS label"));
        }
    }
    if !host.contains('.') {
        return Err(RelayError::invalid(
            "host",
            "must contain at least two DNS labels",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn endpoint_is_strict_and_canonical() -> Result<(), Box<dyn Error>> {
        let endpoint = "Relay.Example.Com:2333".parse::<RelayEndpoint>()?;
        assert_eq!(endpoint.authority(), "relay.example.com:2333");
        for invalid in [
            "https://relay.example.com:2333",
            "192.0.2.1:2333",
            "localhost:2333",
            "ap.nonamenona.top",
            "ap.nonamenona.top:0",
        ] {
            assert!(invalid.parse::<RelayEndpoint>().is_err(), "{invalid}");
        }
        Ok(())
    }
}
