//! Canonical Pairing v1 fixture compatibility.

use std::{error::Error, fs, path::Path};

use agentpulse_pairing::{
    PairingBundle, decode_pairing_request, decode_pairing_uri, decode_server_message,
    encode_pairing_request, encode_server_message,
};
use serde_json::Value;

type TestResult = Result<(), Box<dyn Error>>;

const FIXTURES: [&str; 5] = [
    "pairing_bundle.json",
    "pair_request.json",
    "pairing_pending.json",
    "pairing_succeeded.json",
    "pairing_error.json",
];

#[test]
fn pairing_v1_fixtures_decode_and_reencode_without_semantic_drift() -> TestResult {
    let local = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pairing-v1");
    let canonical =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agentpulse-protocol/fixtures/pairing-v1");

    for name in FIXTURES {
        let bytes = fs::read(local.join(name))?;
        assert_canonical_mirror(&canonical, name, &bytes)?;
    }

    let bundle_bytes = fs::read(local.join("pairing_bundle.json"))?;
    let bundle: PairingBundle = serde_json::from_slice(&bundle_bytes)?;
    assert_eq!(decode_pairing_uri(&bundle.to_uri()?)?, bundle);

    let request_bytes = fs::read(local.join("pair_request.json"))?;
    let request = decode_pairing_request(&request_bytes)?;
    assert_semantic_round_trip(&request_bytes, &encode_pairing_request(&request)?)?;

    for name in [
        "pairing_pending.json",
        "pairing_succeeded.json",
        "pairing_error.json",
    ] {
        let bytes = fs::read(local.join(name))?;
        let message = decode_server_message(&bytes)?;
        assert_semantic_round_trip(&bytes, &encode_server_message(&message)?)?;
    }
    Ok(())
}

fn assert_semantic_round_trip(expected: &[u8], actual: &[u8]) -> TestResult {
    assert_eq!(
        serde_json::from_slice::<Value>(actual)?,
        serde_json::from_slice::<Value>(expected)?,
    );
    Ok(())
}

fn assert_canonical_mirror(canonical: &Path, name: &str, bytes: &[u8]) -> TestResult {
    if canonical.is_dir() {
        assert_eq!(
            bytes,
            fs::read(canonical.join(name))?,
            "fixture mirror drifted: {name}"
        );
    }
    Ok(())
}

#[test]
fn direct_discovery_is_strict_and_preserves_mapped_destination() -> TestResult {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let bytes = include_bytes!("fixtures/pairing-v2/pairing_bundle.json");
    let canonical =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agentpulse-protocol/fixtures/pairing-v2");
    assert_canonical_mirror(&canonical, "pairing_bundle.json", bytes)?;
    let bundle: PairingBundle = serde_json::from_slice(bytes)?;
    let uri = bundle.to_uri()?;
    assert!(uri.starts_with("agentpulse://pair/v2/"));
    assert_eq!(decode_pairing_uri(&uri)?, bundle);
    assert_eq!(bundle.address, "public.example.com");
    assert_eq!(bundle.port, 44321);
    assert!(decode_pairing_uri(&uri.replace("/v2/", "/v1/")).is_err());
    for (key, value) in [
        ("route", serde_json::json!("auto")),
        ("relay_endpoint", serde_json::json!("relay.example.com:443")),
        ("address", serde_json::json!("https://public.example.com")),
        ("address", serde_json::json!("0.0.0.0")),
        ("port", serde_json::json!(0)),
        ("expires_at_unix_seconds", serde_json::json!(1)),
    ] {
        let mut invalid: Value = serde_json::from_slice(bytes)?;
        invalid[key] = value;
        let uri = format!(
            "agentpulse://pair/v2/{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&invalid)?)
        );
        assert!(decode_pairing_uri(&uri).is_err(), "accepted invalid {key}");
    }
    for address in ["203.0.113.10", "2001:db8::1", "public.example.com"] {
        let mut valid = bundle.clone();
        valid.address = address.to_owned();
        assert_eq!(decode_pairing_uri(&valid.to_uri()?)?, valid);
    }
    Ok(())
}
