use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};
use base64::{Engine, engine::general_purpose::STANDARD};
use bridge_core::{
    device::{
        self, ConnectionChallenge, ControllerSession, EnrollmentChallenge, EnrollmentMode,
        Registration,
    },
    identity::{self, OutSigningKey, TenantSeed},
    model::AccountIdentity,
};
use serde_json::{Value, json};
use uuid::Uuid;

const KEY_ID: &str = "DyYm9YYvBNO3c3lVz17Okfg348xMJ9GKIohEcHJ5Qh8";
const IDENTITY_HASH: &str = "egDP_xDz292C7bQUh45gzOO58C7yckQNszYOPpqkG-M";
const NONCE: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
fn key() -> OutSigningKey {
    identity::signing_key(
        &TenantSeed::from_bytes([7; 32]),
        Uuid::from_u128(1),
        &AccountIdentity {
            workspace_id: "workspace-example".into(),
            user_id: "user-example".into(),
        },
        "desk",
    )
    .unwrap()
}
fn registration() -> Registration {
    Registration {
        client_id: "server-issued-client".into(),
        account_user_id: "member-example".into(),
        key_id: KEY_ID.into(),
    }
}
fn enrollment() -> Value {
    json!({
        "purpose":"remote_control_client_enrollment","audience":"remote_control_client_enrollment",
        "account_user_id":"member-example","client_id":"server-issued-client","target_origin":"https://chatgpt.com",
        "target_path":"/backend-api/codex/remote/control/client/enroll/finish","challenge_expires_at":1800000300,
        "nonce":NONCE,"challenge_id":"challenge-example","challenge_token":"synthetic-challenge-token"
    })
}
fn verify(key: &OutSigningKey, signed: &str, signature: &str) -> Vec<u8> {
    let raw = STANDARD.decode(signed).unwrap();
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key.public_key())
        .verify(&raw, &STANDARD.decode(signature).unwrap())
        .unwrap();
    raw
}

#[test]
fn public_identity_matches_independent_bun_hash_vector() {
    let identity = device::identity(&key());
    assert_eq!(identity.key_id, KEY_ID);
    assert_eq!(
        identity.public_key_spki_der_base64,
        "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEmny/D07bcHhCyYWzOX/C/amj7NwqfDiGozZgedUOOxfE7kXohePCWb3sQEzu0IPbh/BJG3/QSSeHLbKVS3TmtA=="
    );
    assert_eq!(device::identity_hash(&key()), IDENTITY_HASH);
}

#[test]
fn enrollment_proof_has_exact_desktop_payload_and_binds_every_challenge_field() {
    let key = key();
    let registration = registration();
    let challenge: EnrollmentChallenge = serde_json::from_value(enrollment()).unwrap();
    let proof = device::enrollment_proof(
        &key,
        &registration,
        &challenge,
        EnrollmentMode::Enroll,
        1800000000,
    )
    .unwrap();
    let raw = verify(
        &key,
        &proof.signed_payload_base64,
        &proof.signature_der_base64,
    );
    let expected = format!(
        "{{\"domain\":\"codex-device-key-sign-payload/v1\",\"payload\":{{\"accountUserId\":\"member-example\",\"audience\":\"remote_control_client_enrollment\",\"challengeExpiresAt\":1800000300,\"challengeId\":\"challenge-example\",\"clientId\":\"server-issued-client\",\"deviceIdentitySha256Base64url\":\"{IDENTITY_HASH}\",\"nonce\":\"{NONCE}\",\"targetOrigin\":\"https://chatgpt.com\",\"targetPath\":\"/backend-api/codex/remote/control/client/enroll/finish\",\"type\":\"remoteControlClientEnrollment\"}}}}"
    );
    assert_eq!(raw, expected.as_bytes());
    for (field, value) in [
        ("purpose", json!("other")),
        ("audience", json!("other")),
        ("account_user_id", json!("other")),
        ("client_id", json!("other")),
        ("target_origin", json!("https://attacker.example")),
        ("target_path", json!("/other")),
        ("challenge_expires_at", json!(1800000000)),
        ("device_identity_hash", json!("other")),
        ("nonce", json!("AA")),
    ] {
        let mut bad = enrollment();
        bad[field] = value;
        let challenge = serde_json::from_value(bad).unwrap();
        assert!(
            device::enrollment_proof(
                &key,
                &registration,
                &challenge,
                EnrollmentMode::Enroll,
                1800000000
            )
            .is_err(),
            "{field}"
        );
    }
    let mut refresh = enrollment();
    refresh["target_path"] = json!("/backend-api/codex/remote/control/client/refresh/finish");
    assert!(
        device::enrollment_proof(
            &key,
            &registration,
            &serde_json::from_value(refresh.clone()).unwrap(),
            EnrollmentMode::Refresh,
            1800000000
        )
        .is_err()
    );
    refresh["device_identity_hash"] = json!(IDENTITY_HASH);
    refresh["challenge_expires_at"] = json!(1800000300.25);
    assert!(
        device::enrollment_proof(
            &key,
            &registration,
            &serde_json::from_value(refresh).unwrap(),
            EnrollmentMode::Refresh,
            1800000000
        )
        .is_ok()
    );
}

#[test]
fn connection_proof_binds_session_token_and_never_signs_a_foreign_owner() {
    let session: ControllerSession = serde_json::from_value(json!({"client_id":"server-issued-client","account_user_id":"member-example",
        "scopes":["remote_control_controller_websocket"],"expires_at":"2099-01-01T00:00:00Z","remote_control_token":"synthetic-session-token"})).unwrap();
    let value = json!({"type":"device_key_challenge","purpose":"remote_control_client_websocket","audience":"remote_control_client_websocket",
        "targetOrigin":"https://chatgpt.com","targetPath":"/backend-api/codex/remote/control/client","accountUserId":"member-example","clientId":"server-issued-client",
        "tokenSha256Base64url":device::digest_url(b"synthetic-session-token"),"tokenExpiresAt":session.expiry().unwrap(),
        "scopes":["remote_control_controller_websocket"],"nonce":NONCE,"sessionId":"session-example"});
    let challenge: ConnectionChallenge = serde_json::from_value(value.clone()).unwrap();
    let proof = device::connection_proof(&key(), &registration(), &session, &challenge, 1800000000)
        .unwrap();
    let raw = verify(
        &key(),
        &proof.signature.signed_payload_base64,
        &proof.signature.signature_der_base64,
    );
    let signed: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(signed["payload"]["type"], "remoteControlClientConnection");
    assert_eq!(signed["payload"]["sessionId"], "session-example");
    assert!(signed["payload"].get("purpose").is_none());
    assert_eq!(
        serde_json::to_value(proof).unwrap()["type"],
        "device_key_proof"
    );
    for (field, bad) in [
        ("clientId", json!("other")),
        ("accountUserId", json!("other")),
        ("tokenSha256Base64url", json!("other")),
        ("tokenExpiresAt", json!(session.expiry().unwrap() + 1)),
        ("scopes", json!(["extra"])),
        ("nonce", json!("AA")),
        ("sessionId", json!("")),
    ] {
        let mut changed = value.clone();
        changed[field] = bad;
        assert!(
            device::connection_proof(
                &key(),
                &registration(),
                &session,
                &serde_json::from_value(changed).unwrap(),
                1800000000
            )
            .is_err(),
            "{field}"
        );
    }
}
