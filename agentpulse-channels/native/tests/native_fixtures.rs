//! Canonical Native Transport v3 fixture compatibility.

use std::{error::Error, fs, path::Path};

use agentpulse_channel_native::{
    decode_client_message, decode_server_message, encode_client_message, encode_server_message,
};
use serde_json::Value;

type TestResult = Result<(), Box<dyn Error>>;

const CLIENT_FIXTURES: [&str; 6] = [
    "client_hello.json",
    "discover_sessions.json",
    "subscribe_session.json",
    "unsubscribe_session.json",
    "submit_interaction_response.json",
    "submit_command.json",
];

const SERVER_FIXTURES: [&str; 11] = [
    "server_hello.json",
    "sync_started.json",
    "discovery_session.json",
    "sync_completed.json",
    "subscription_result.json",
    "subscription_interaction.json",
    "subscription_form.json",
    "interaction_response_result.json",
    "command_result.json",
    "unsubscription_result.json",
    "error.json",
];

#[test]
fn native_v3_fixtures_decode_and_reencode_without_semantic_drift() -> TestResult {
    let local = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native-v3");
    let canonical = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../agentpulse-protocol/fixtures/native-v3");

    for name in CLIENT_FIXTURES {
        let bytes = fs::read(local.join(name))?;
        let expected: Value = serde_json::from_slice(&bytes)?;
        let message = decode_client_message(&bytes)?;
        let actual: Value = serde_json::from_slice(&encode_client_message(&message)?)?;
        assert_eq!(actual, expected, "fixture changed semantically: {name}");
        assert_canonical_mirror(&canonical, name, &bytes)?;
    }

    for name in SERVER_FIXTURES {
        let bytes = fs::read(local.join(name))?;
        let expected: Value = serde_json::from_slice(&bytes)?;
        let message = decode_server_message(&bytes)?;
        let actual: Value = serde_json::from_slice(&encode_server_message(&message)?)?;
        assert_eq!(actual, expected, "fixture changed semantically: {name}");
        assert_canonical_mirror(&canonical, name, &bytes)?;
    }
    Ok(())
}

fn assert_canonical_mirror(canonical: &Path, name: &str, bytes: &[u8]) -> TestResult {
    if canonical.is_dir() {
        let canonical_bytes = fs::read(canonical.join(name))?;
        assert_eq!(bytes, canonical_bytes, "fixture mirror drifted: {name}");
    }
    Ok(())
}
