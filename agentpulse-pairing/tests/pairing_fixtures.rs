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
