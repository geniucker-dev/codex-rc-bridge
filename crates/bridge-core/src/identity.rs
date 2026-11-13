//! Immutable derivation contract; see docs/agent/IDENTITY.md.
use crate::model::{AccountIdentity, Endpoint, EndpointKind};
use aws_lc_rs::{
    hkdf, rand,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair},
};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error(
        "identity fields must be nonempty, without surrounding whitespace or control characters"
    )]
    InvalidField,
    #[error("identity cryptography failed")]
    Crypto,
}

// Intentionally neither Debug nor Serialize. Ownership and zeroization stay in the core.
pub struct TenantSeed(Zeroizing<[u8; 32]>);
impl TenantSeed {
    pub fn generate() -> Result<Self, IdentityError> {
        let mut bytes = Zeroizing::new([0; 32]);
        rand::fill(bytes.as_mut()).map_err(|_| IdentityError::Crypto)?;
        Ok(Self(bytes))
    }
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }
    pub(crate) fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub struct OutSigningKey(EcdsaKeyPair);
impl OutSigningKey {
    pub fn public_key(&self) -> &[u8] {
        self.0.public_key().as_ref()
    }
    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>, IdentityError> {
        self.0
            .sign(&rand::SystemRandom::new(), message)
            .map(|s| s.as_ref().to_vec())
            .map_err(|_| IdentityError::Crypto)
    }
}

pub fn validate_field(value: &str) -> Result<(), IdentityError> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().any(char::is_control)
        || value.len() > 4096
    {
        return Err(IdentityError::InvalidField);
    }
    Ok(())
}
fn context(tenant: Uuid, account: &AccountIdentity, name: &str) -> Result<Vec<u8>, IdentityError> {
    for field in [&account.workspace_id, &account.user_id, name] {
        validate_field(field)?;
    }
    let mut bytes = Vec::new();
    for field in [
        tenant.as_bytes().as_slice(),
        account.workspace_id.as_bytes(),
        account.user_id.as_bytes(),
        name.as_bytes(),
    ] {
        bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    Ok(bytes)
}
struct Length(usize);
impl hkdf::KeyType for Length {
    fn len(&self) -> usize {
        self.0
    }
}
fn derive(
    seed: &TenantSeed,
    label: &[u8],
    context: &[u8],
    counter: Option<u32>,
    out: &mut [u8],
) -> Result<(), IdentityError> {
    let prk =
        hkdf::Salt::new(hkdf::HKDF_SHA256, b"codex-rc-bridge/identity").extract(seed.0.as_ref());
    let mut info = Vec::new();
    info.extend_from_slice(&(label.len() as u32).to_be_bytes());
    info.extend_from_slice(label);
    info.extend_from_slice(context);
    if let Some(counter) = counter {
        info.extend_from_slice(&counter.to_be_bytes());
    }
    prk.expand(&[&info], Length(out.len()))
        .map_err(|_| IdentityError::Crypto)?
        .fill(out)
        .map_err(|_| IdentityError::Crypto)
}
pub fn endpoint(
    seed: &TenantSeed,
    tenant: Uuid,
    account: AccountIdentity,
    name: &str,
    kind: EndpointKind,
) -> Result<Endpoint, IdentityError> {
    let context = context(tenant, &account, name)?;
    let label: &[u8] = match kind {
        EndpointKind::In => b"in/id",
        EndpointKind::Out => b"out/id",
    };
    let mut bytes = [0; 16];
    derive(seed, label, &context, None, &mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Endpoint {
        tenant_id: tenant,
        device_id: Uuid::from_bytes(bytes),
        account,
        kind,
        identity_name: name.to_owned(),
        display_name: name.to_owned(),
    })
}
pub fn signing_key(
    seed: &TenantSeed,
    tenant: Uuid,
    account: &AccountIdentity,
    name: &str,
) -> Result<OutSigningKey, IdentityError> {
    let context = context(tenant, account, name)?;
    const ORDER: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63,
        0x25, 0x51,
    ];
    for counter in 0..=u32::MAX {
        let mut scalar = Zeroizing::new([0; 32]);
        derive(
            seed,
            b"out/signing",
            &context,
            Some(counter),
            scalar.as_mut(),
        )?;
        if scalar.iter().all(|b| *b == 0) || *scalar >= ORDER {
            continue;
        }
        // RFC 5915 ECPrivateKey with namedCurve prime256v1; AWS-LC computes Q = dG.
        let mut der = Zeroizing::new(vec![0x30, 0x31, 0x02, 0x01, 0x01, 0x04, 0x20]);
        der.extend_from_slice(scalar.as_ref());
        der.extend_from_slice(&[
            0xa0, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
        ]);
        return EcdsaKeyPair::from_private_key_der(&ECDSA_P256_SHA256_ASN1_SIGNING, &der)
            .map(OutSigningKey)
            .map_err(|_| IdentityError::Crypto);
    }
    Err(IdentityError::Crypto)
}
