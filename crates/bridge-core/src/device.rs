//! Desktop-compatible device identities and domain-separated enrollment/WS proofs.
use crate::{
    auth::{AccountTokens, SecretString, validate_step_up},
    identity::OutSigningKey,
};
use aws_lc_rs::digest;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};

const ALGORITHM: &str = "ecdsa_p256_sha256";
const PROTECTION: &str = "os_protected_nonextractable";
const ORIGIN: &str = "https://chatgpt.com";
pub const CLIENT_PATH: &str = "/backend-api/codex/remote/control/client";
pub const CONTROLLER_SCOPE: &str = "remote_control_controller_websocket";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeviceError {
    #[error("device challenge identity or scope does not match")]
    Binding,
    #[error("device challenge has expired or has invalid metadata")]
    Challenge,
    #[error("device signature failed")]
    Signature,
    #[error("fresh controller authorization is required")]
    StepUp,
}
pub fn digest_url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, bytes))
}

#[derive(Serialize)]
pub struct DeviceIdentity {
    pub key_id: String,
    pub public_key_spki_der_base64: String,
    pub algorithm: &'static str,
    pub protection_class: &'static str,
}
pub fn identity(key: &OutSigningKey) -> DeviceIdentity {
    // RFC 5480 SubjectPublicKeyInfo: id-ecPublicKey + prime256v1 + uncompressed Q.
    let mut spki = vec![
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];
    spki.extend_from_slice(key.public_key());
    DeviceIdentity {
        key_id: digest_url(&spki),
        public_key_spki_der_base64: STANDARD.encode(spki),
        algorithm: ALGORITHM,
        protection_class: PROTECTION,
    }
}
pub fn identity_hash(key: &OutSigningKey) -> String {
    // Hash in declaration order with camelCase keys; a sorted map would change the hash.
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HashIdentity<'a> {
        algorithm: &'a str,
        key_id: &'a str,
        protection_class: &'a str,
        public_key_spki_der_base64: &'a str,
    }
    let identity = identity(key);
    digest_url(
        &serde_json::to_vec(&HashIdentity {
            algorithm: ALGORITHM,
            key_id: &identity.key_id,
            protection_class: PROTECTION,
            public_key_spki_der_base64: &identity.public_key_spki_der_base64,
        })
        .expect("serializing string fields"),
    )
}

#[derive(Serialize, Deserialize)]
pub struct Registration {
    pub client_id: String,
    pub account_user_id: String,
    pub key_id: String,
}
#[derive(Deserialize)]
pub struct EnrollmentChallenge {
    pub purpose: String,
    pub audience: String,
    pub account_user_id: String,
    pub client_id: String,
    pub target_origin: String,
    pub target_path: String,
    pub challenge_expires_at: serde_json::Number,
    pub device_identity_hash: Option<String>,
    pub nonce: String,
    pub challenge_id: String,
    pub challenge_token: SecretString,
}
#[derive(Clone, Copy)]
pub enum EnrollmentMode {
    Enroll,
    Refresh,
}
impl EnrollmentMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Enroll => "enroll",
            Self::Refresh => "refresh",
        }
    }
}
#[derive(Serialize)]
pub struct EnrollmentProof {
    pub challenge_token: SecretString,
    pub key_id: String,
    pub signature_der_base64: String,
    pub signed_payload_base64: String,
    pub algorithm: &'static str,
}
fn valid_nonce(nonce: &str) -> bool {
    nonce.len() <= 4096
        && URL_SAFE_NO_PAD
            .decode(nonce.trim_end_matches('='))
            .is_ok_and(|n| n.len() >= 32)
}
pub fn enrollment_proof(
    key: &OutSigningKey,
    registration: &Registration,
    challenge: &EnrollmentChallenge,
    mode: EnrollmentMode,
    now: u64,
) -> Result<EnrollmentProof, DeviceError> {
    let path = format!("{CLIENT_PATH}/{}/finish", mode.name());
    let hash = identity_hash(key);
    if registration.key_id != identity(key).key_id
        || challenge.purpose != "remote_control_client_enrollment"
        || challenge.audience != "remote_control_client_enrollment"
        || challenge.account_user_id != registration.account_user_id
        || challenge.client_id != registration.client_id
        || challenge.target_origin != ORIGIN
        || challenge.target_path != path
        || ((matches!(mode, EnrollmentMode::Refresh) || challenge.device_identity_hash.is_some())
            && challenge.device_identity_hash.as_deref() != Some(hash.as_str()))
    {
        return Err(DeviceError::Binding);
    }
    if challenge
        .challenge_expires_at
        .as_f64()
        .is_none_or(|expiry| !expiry.is_finite() || expiry <= now as f64)
        || challenge.challenge_id.is_empty()
        || challenge.challenge_token.expose().is_empty()
        || !valid_nonce(&challenge.nonce)
    {
        return Err(DeviceError::Challenge);
    }
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Payload<'a> {
        account_user_id: &'a str,
        audience: &'a str,
        challenge_expires_at: &'a serde_json::Number,
        challenge_id: &'a str,
        client_id: &'a str,
        device_identity_sha256_base64url: &'a str,
        nonce: &'a str,
        target_origin: &'a str,
        target_path: &'a str,
        r#type: &'a str,
    }
    let signed = sign(
        key,
        &Payload {
            account_user_id: &registration.account_user_id,
            audience: &challenge.audience,
            challenge_expires_at: &challenge.challenge_expires_at,
            challenge_id: &challenge.challenge_id,
            client_id: &registration.client_id,
            device_identity_sha256_base64url: &hash,
            nonce: &challenge.nonce,
            target_origin: ORIGIN,
            target_path: &path,
            r#type: "remoteControlClientEnrollment",
        },
    )?;
    Ok(EnrollmentProof {
        challenge_token: challenge.challenge_token.clone(),
        key_id: signed.key_id,
        signature_der_base64: signed.signature_der_base64,
        signed_payload_base64: signed.signed_payload_base64,
        algorithm: ALGORITHM,
    })
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    pub key_id: String,
    pub signature_der_base64: String,
    pub signed_payload_base64: String,
    pub algorithm: &'static str,
}
fn sign(key: &OutSigningKey, payload: &impl Serialize) -> Result<Signature, DeviceError> {
    #[derive(Serialize)]
    struct Domain<'a, T> {
        domain: &'static str,
        payload: &'a T,
    }
    let raw = serde_json::to_vec(&Domain {
        domain: "codex-device-key-sign-payload/v1",
        payload,
    })
    .map_err(|_| DeviceError::Signature)?;
    let signature = key.sign(&raw).map_err(|_| DeviceError::Signature)?;
    Ok(Signature {
        key_id: identity(key).key_id,
        signature_der_base64: STANDARD.encode(signature),
        signed_payload_base64: STANDARD.encode(raw),
        algorithm: ALGORITHM,
    })
}

#[derive(Deserialize)]
pub struct ControllerSession {
    pub client_id: String,
    pub account_user_id: String,
    pub scopes: Vec<String>,
    pub expires_at: String,
    pub remote_control_token: SecretString,
}
impl ControllerSession {
    pub fn expiry(&self) -> Result<i64, DeviceError> {
        time::OffsetDateTime::parse(
            &self.expires_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map(|t| t.unix_timestamp())
        .map_err(|_| DeviceError::Challenge)
    }
    pub fn validate(&self, registration: &Registration, now: i64) -> Result<(), DeviceError> {
        if self.client_id != registration.client_id
            || self.account_user_id != registration.account_user_id
            || self.scopes != [CONTROLLER_SCOPE]
            || self.expiry()? <= now
            || self.remote_control_token.expose().is_empty()
        {
            return Err(DeviceError::Binding);
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionChallenge {
    pub r#type: String,
    pub purpose: String,
    pub audience: String,
    pub target_origin: String,
    pub target_path: String,
    pub account_user_id: String,
    pub client_id: String,
    pub token_sha256_base64url: String,
    pub token_expires_at: i64,
    pub scopes: Vec<String>,
    pub nonce: String,
    pub session_id: String,
}
#[derive(Serialize)]
pub struct ConnectionProof {
    pub r#type: &'static str,
    #[serde(flatten)]
    pub signature: Signature,
}
pub fn connection_proof(
    key: &OutSigningKey,
    registration: &Registration,
    session: &ControllerSession,
    challenge: &ConnectionChallenge,
    now: i64,
) -> Result<ConnectionProof, DeviceError> {
    session.validate(registration, now)?;
    if registration.key_id != identity(key).key_id
        || challenge.r#type != "device_key_challenge"
        || challenge.purpose != "remote_control_client_websocket"
        || challenge.audience != "remote_control_client_websocket"
        || challenge.target_origin != ORIGIN
        || challenge.target_path != CLIENT_PATH
        || challenge.account_user_id != registration.account_user_id
        || challenge.client_id != registration.client_id
        || challenge.token_sha256_base64url
            != digest_url(session.remote_control_token.expose().as_bytes())
        || challenge.token_expires_at != session.expiry()?
        || challenge.scopes != [CONTROLLER_SCOPE]
    {
        return Err(DeviceError::Binding);
    }
    if !valid_nonce(&challenge.nonce) || challenge.session_id.is_empty() {
        return Err(DeviceError::Challenge);
    }
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Payload<'a> {
        account_user_id: &'a str,
        audience: &'a str,
        client_id: &'a str,
        nonce: &'a str,
        scopes: &'a [String],
        session_id: &'a str,
        target_origin: &'a str,
        target_path: &'a str,
        token_expires_at: i64,
        token_sha256_base64url: &'a str,
        r#type: &'a str,
    }
    Ok(ConnectionProof {
        r#type: "device_key_proof",
        signature: sign(
            key,
            &Payload {
                account_user_id: &challenge.account_user_id,
                audience: &challenge.audience,
                client_id: &challenge.client_id,
                nonce: &challenge.nonce,
                scopes: &challenge.scopes,
                session_id: &challenge.session_id,
                target_origin: ORIGIN,
                target_path: CLIENT_PATH,
                token_expires_at: challenge.token_expires_at,
                token_sha256_base64url: &challenge.token_sha256_base64url,
                r#type: "remoteControlClientConnection",
            },
        )?,
    })
}
pub fn authorize_enrollment(
    token: &SecretString,
    account: &AccountTokens,
    registration: &Registration,
    now: time::OffsetDateTime,
) -> Result<(), DeviceError> {
    validate_step_up(token, account, &registration.account_user_id, now)
        .map_err(|_| DeviceError::StepUp)
}
