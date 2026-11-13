//! Checkpoint OUT identity before enroll/finish can grant control. Recovery never creates a new key.
use crate::{
    auth::{AuthError, AuthService, SecretString, validate_step_up_for_account},
    device::{self, Registration},
    identity::{self, OutSigningKey, TenantSeed},
    model::{Endpoint, EndpointKind},
    storage::StorageError,
    upstream::{ControlError, Upstream, unix_time},
    vault::{SecretScope, Vault, VaultError},
};

#[derive(Debug, thiserror::Error)]
pub enum EnrollmentError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("registration checkpoint is unavailable or does not match this device")]
    State,
}
async fn key(vault: &Vault, endpoint: &Endpoint) -> Result<OutSigningKey, EnrollmentError> {
    if endpoint.kind != EndpointKind::Out {
        return Err(EnrollmentError::State);
    }
    let seed = vault
        .read(endpoint.tenant_id, &SecretScope::TenantSeed)
        .await?
        .ok_or(EnrollmentError::State)?;
    let seed = TenantSeed::from_bytes(
        seed.as_slice()
            .try_into()
            .map_err(|_| EnrollmentError::State)?,
    );
    let expected = identity::endpoint(
        &seed,
        endpoint.tenant_id,
        endpoint.account.clone(),
        &endpoint.identity_name,
        EndpointKind::Out,
    )
    .map_err(|_| EnrollmentError::State)?;
    if expected.device_id != endpoint.device_id {
        return Err(EnrollmentError::State);
    }
    identity::signing_key(
        &seed,
        endpoint.tenant_id,
        &endpoint.account,
        &endpoint.identity_name,
    )
    .map_err(|_| EnrollmentError::State)
}
pub async fn register_controller(
    vault: &Vault,
    auth: &AuthService<Upstream>,
    upstream: &Upstream,
    endpoint: &Endpoint,
    step_up: &SecretString,
) -> Result<Registration, EnrollmentError> {
    auth.ensure_endpoint_usable(endpoint).await?;
    let key = key(vault, endpoint).await?;
    let credentials = auth
        .account(endpoint.tenant_id, &endpoint.account, None, unix_time())
        .await?;
    validate_step_up_for_account(step_up, &credentials, time::OffsetDateTime::now_utc())?;
    vault
        .store
        .begin_enrollment(endpoint.tenant_id, endpoint.device_id)
        .await?;
    let enrollment = upstream
        .start_controller_enrollment(auth, endpoint.tenant_id, &endpoint.account, &key)
        .await
        .map_err(crate::upstream::controller_authorization_error)?;
    // Persist the returned identity before further validation/proof work can fail.
    // Do not send finish if this commit fails. No step-up token or proof is stored.
    vault
        .save_out_enrollment(
            endpoint.tenant_id,
            endpoint.device_id,
            &enrollment.registration,
        )
        .await?;
    let registration = upstream
        .finish_controller_enrollment(
            auth,
            endpoint.tenant_id,
            &endpoint.account,
            &key,
            step_up,
            enrollment,
        )
        .await
        .map_err(crate::upstream::controller_authorization_error)?;
    vault
        .save_out_registration(endpoint.tenant_id, endpoint.device_id, &registration)
        .await?;
    Ok(registration)
}

/// Restore evidence for an uncertain OUT using a client ID recovered by its owner.
/// Caller holds the tenant coordinator or exclusive process lock throughout.
pub async fn restore_controller_record(
    vault: &Vault,
    auth: &AuthService<Upstream>,
    upstream: &Upstream,
    endpoint: &Endpoint,
    client_id: &str,
) -> Result<(), EnrollmentError> {
    let eligible: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints e JOIN enrollment_attempts a ON a.tenant_id=e.tenant_id AND a.device_id=e.device_id WHERE e.tenant_id=? AND e.device_id=? AND e.kind='out' AND e.lifecycle IN ('draft','deleting') AND NOT EXISTS(SELECT 1 FROM bindings WHERE tenant_id=e.tenant_id AND out_id=e.device_id))")
        .bind(endpoint.tenant_id.to_string()).bind(endpoint.device_id.to_string())
        .fetch_one(&vault.store.pool).await.map_err(StorageError::from)?;
    if !eligible {
        return Err(EnrollmentError::State);
    }
    for scope in [
        SecretScope::OutEnrollment(endpoint.device_id),
        SecretScope::OutRegistration(endpoint.device_id),
    ] {
        if vault.read(endpoint.tenant_id, &scope).await?.is_some() {
            return Err(EnrollmentError::State);
        }
    }
    let key = key(vault, endpoint).await?;
    let verified = upstream
        .prepare_controller_refresh(auth, endpoint.tenant_id, &endpoint.account, &key, client_id)
        .await?;
    // No refresh/finish or enroll is sent. Saving evidence cannot reactivate a deleting OUT.
    vault
        .restore_out_enrollment(
            endpoint.tenant_id,
            endpoint.device_id,
            &verified.registration,
        )
        .await?;
    Ok(())
}

/// Only an authenticated, original checkpoint can be used for recovery or scoped deletion.
pub(crate) async fn pending_registration(
    vault: &Vault,
    auth: &AuthService<Upstream>,
    endpoint: &Endpoint,
) -> Result<Option<Registration>, EnrollmentError> {
    let Some(raw) = vault
        .read(
            endpoint.tenant_id,
            &SecretScope::OutEnrollment(endpoint.device_id),
        )
        .await?
    else {
        return Ok(None);
    };
    let registration: Registration =
        serde_json::from_slice(&raw).map_err(|_| EnrollmentError::State)?;
    let key = key(vault, endpoint).await?;
    let credentials = auth
        .account(endpoint.tenant_id, &endpoint.account, None, unix_time())
        .await?;
    if registration.key_id != device::identity(&key).key_id
        || !credentials.accepts_owner(&registration.account_user_id)
    {
        return Err(EnrollmentError::State);
    }
    Ok(Some(registration))
}

pub async fn recover_controller(
    vault: &Vault,
    auth: &AuthService<Upstream>,
    upstream: &Upstream,
    endpoint: &Endpoint,
) -> Result<(), EnrollmentError> {
    auth.ensure_endpoint_usable(endpoint).await?;
    let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints e JOIN enrollment_attempts a ON a.tenant_id=e.tenant_id AND a.device_id=e.device_id WHERE e.tenant_id=? AND e.device_id=? AND e.kind='out' AND e.lifecycle IN ('draft','active'))")
        .bind(endpoint.tenant_id.to_string()).bind(endpoint.device_id.to_string()).fetch_one(&vault.store.pool).await.map_err(StorageError::from)?;
    if !pending {
        return Err(EnrollmentError::State);
    }
    let registration = pending_registration(vault, auth, endpoint)
        .await?
        .ok_or(EnrollmentError::State)?;
    let key = key(vault, endpoint).await?;
    // A refresh proves the original key against the same account/client/challenge. Never re-enroll here.
    upstream
        .refresh_controller(
            auth,
            endpoint.tenant_id,
            &endpoint.account,
            &key,
            &registration,
        )
        .await?;
    vault
        .save_out_registration(endpoint.tenant_id, endpoint.device_id, &registration)
        .await?;
    Ok(())
}
