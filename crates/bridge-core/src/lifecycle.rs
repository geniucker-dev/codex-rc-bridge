//! Revocation is separate from stopping/unbinding. All calls belong to the single
//! tenant coordinator or exclusive local operator; never run authorization in parallel.
use crate::{
    auth::{AuthError, AuthService},
    model::{Endpoint, EndpointKind},
    runtime::{RuntimeError, load_registration},
    storage::{SqliteStore, StorageError},
    upstream::{ControlError, HostRegistration, HostSession, PairingCode, Upstream},
    vault::{SecretScope, Vault, scope_key},
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum DeleteError {
    #[error(transparent)]
    Enrollment(#[from] crate::enrollment::EnrollmentError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error("endpoint cleanup state is invalid or still in use")]
    State,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", content = "until", rename_all = "snake_case")]
pub enum DeleteOutcome {
    Complete,
    RegistrationUnknown,
    RevocationPending,
}

impl SqliteStore {
    pub(crate) async fn enrollment_pending(
        &self,
        tenant: Uuid,
        device: Uuid,
    ) -> Result<bool, StorageError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM enrollment_attempts WHERE tenant_id=? AND device_id=?)",
        )
        .bind(tenant.to_string())
        .bind(device.to_string())
        .fetch_one(&self.pool)
        .await?)
    }
    /// OUT write-ahead record: retain unknown controller authorization for recovery.
    pub async fn begin_enrollment(&self, tenant: Uuid, device: Uuid) -> Result<(), StorageError> {
        let changed=sqlx::query("INSERT INTO enrollment_attempts(tenant_id,device_id) SELECT tenant_id,device_id FROM endpoints WHERE tenant_id=? AND device_id=? AND kind='out' AND lifecycle IN ('draft','active')")
            .bind(tenant.to_string()).bind(device.to_string()).execute(&self.pool).await?.rows_affected();
        if changed != 1 {
            return Err(StorageError::Invalid);
        }
        Ok(())
    }
    pub async fn endpoint_deleting(
        &self,
        tenant: Uuid,
        device: Uuid,
    ) -> Result<bool, StorageError> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints WHERE tenant_id=? AND device_id=? AND lifecycle='deleting')")
            .bind(tenant.to_string()).bind(device.to_string()).fetch_one(&self.pool).await?)
    }
    pub async fn deleting_endpoints(&self, tenant: Uuid) -> Result<Vec<Endpoint>, StorageError> {
        let ids:Vec<String>=sqlx::query_scalar("SELECT device_id FROM endpoints WHERE tenant_id=? AND lifecycle='deleting' ORDER BY device_id")
            .bind(tenant.to_string()).fetch_all(&self.pool).await?;
        let mut result = Vec::new();
        for id in ids {
            result.push(
                self.endpoint(tenant, id.parse().map_err(|_| StorageError::Invalid)?)
                    .await?
                    .ok_or(StorageError::Invalid)?,
            );
        }
        Ok(result)
    }
    pub(crate) async fn begin_delete(&self, endpoint: &Endpoint) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await?;
        let switching:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chain_switches WHERE tenant_id=? AND (in_id=? OR old_out_id=? OR new_out_id=?))")
            .bind(endpoint.tenant_id.to_string()).bind(endpoint.device_id.to_string()).bind(endpoint.device_id.to_string()).bind(endpoint.device_id.to_string()).fetch_one(&mut *tx).await?;
        if switching {
            return Err(StorageError::Invalid);
        }
        let scope = match endpoint.kind {
            EndpointKind::In => SecretScope::InRegistration(endpoint.device_id),
            EndpointKind::Out => SecretScope::OutRegistration(endpoint.device_id),
        };
        // Preserve evidence that this IN was active. With neither registration nor
        // environment, it cannot be treated as an unfinished, never-paired draft.
        let missing_in_identity: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints e WHERE e.tenant_id=? AND e.device_id=? AND e.kind='in' AND e.lifecycle='active' AND NOT EXISTS(SELECT 1 FROM secrets s WHERE s.tenant_id=e.tenant_id AND s.scope=?) AND NOT EXISTS(SELECT 1 FROM host_environments h WHERE h.tenant_id=e.tenant_id AND h.in_id=e.device_id))")
            .bind(endpoint.tenant_id.to_string()).bind(endpoint.device_id.to_string())
            .bind(scope_key(&scope).map_err(|_| StorageError::Invalid)?)
            .fetch_one(&mut *tx).await?;
        if missing_in_identity {
            return Err(StorageError::Invalid);
        }
        // Active metadata without its protected registration is damaged state, not
        // evidence that no remote permissions exist. Retain a durable cleanup barrier.
        sqlx::query("INSERT INTO enrollment_attempts(tenant_id,device_id) SELECT tenant_id,device_id FROM endpoints WHERE tenant_id=? AND device_id=? AND kind='out' AND lifecycle='active' AND NOT EXISTS(SELECT 1 FROM secrets WHERE tenant_id=? AND scope=?) ON CONFLICT DO NOTHING")
            .bind(endpoint.tenant_id.to_string()).bind(endpoint.device_id.to_string()).bind(endpoint.tenant_id.to_string()).bind(scope_key(&scope).map_err(|_|StorageError::Invalid)?)
            .execute(&mut *tx).await?;
        let changed = sqlx::query(
            "UPDATE endpoints SET lifecycle='deleting' WHERE tenant_id=? AND device_id=?",
        )
        .bind(endpoint.tenant_id.to_string())
        .bind(endpoint.device_id.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(StorageError::Invalid);
        }
        sqlx::query("UPDATE chain_settings SET enabled=0 WHERE tenant_id=? AND in_id IN (SELECT in_id FROM bindings WHERE tenant_id=? AND (in_id=? OR out_id=?))")
            .bind(endpoint.tenant_id.to_string()).bind(endpoint.tenant_id.to_string()).bind(endpoint.device_id.to_string()).bind(endpoint.device_id.to_string()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

/// The tenant coordinator serializes pairing with endpoint deletion.
pub async fn pair_host(
    store: &SqliteStore,
    upstream: &Upstream,
    tenant: Uuid,
    input: Uuid,
    session: &HostSession,
) -> Result<PairingCode, DeleteError> {
    let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints e JOIN host_environments h ON h.tenant_id=e.tenant_id AND h.in_id=e.device_id WHERE e.tenant_id=? AND e.device_id=? AND e.kind='in' AND e.lifecycle='active' AND h.environment_id=?)")
        .bind(tenant.to_string()).bind(input.to_string()).bind(&session.environment_id)
        .fetch_one(&store.pool).await.map_err(StorageError::from)?;
    if !active {
        return Err(DeleteError::State);
    }
    Ok(upstream.pair_host(session).await?)
}

/// Called after the related runtime has joined and its binding has been removed.
/// Failures retain `deleting`, credentials and identity metadata for an explicit retry.
pub(crate) async fn cleanup(
    vault: &Vault,
    auth: &AuthService<Upstream>,
    upstream: &Upstream,
    endpoint: &Endpoint,
) -> Result<DeleteOutcome, DeleteError> {
    let store = &vault.store;
    if !store
        .endpoint_deleting(endpoint.tenant_id, endpoint.device_id)
        .await?
    {
        return Err(DeleteError::State);
    }
    let bound: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM bindings WHERE tenant_id=? AND (in_id=? OR out_id=?))",
    )
    .bind(endpoint.tenant_id.to_string())
    .bind(endpoint.device_id.to_string())
    .bind(endpoint.device_id.to_string())
    .fetch_one(&store.pool)
    .await
    .map_err(StorageError::from)?;
    if bound {
        return Err(DeleteError::State);
    }
    auth.invalidate_endpoint(endpoint.tenant_id, endpoint.device_id)
        .await?;
    if endpoint.kind == EndpointKind::Out
        && store
            .enrollment_pending(endpoint.tenant_id, endpoint.device_id)
            .await?
    {
        let Some(registration) =
            crate::enrollment::pending_registration(vault, auth, endpoint).await?
        else {
            return Ok(DeleteOutcome::RegistrationUnknown);
        };
        upstream
            .revoke_controller(
                auth,
                endpoint.tenant_id,
                &endpoint.account,
                &registration.client_id,
            )
            .await?;
        finish_local(
            vault,
            endpoint,
            &SecretScope::OutRegistration(endpoint.device_id),
        )
        .await?;
        return Ok(DeleteOutcome::Complete);
    }
    let scope = match endpoint.kind {
        EndpointKind::In => SecretScope::InRegistration(endpoint.device_id),
        EndpointKind::Out => SecretScope::OutRegistration(endpoint.device_id),
    };
    match endpoint.kind {
        EndpointKind::Out => {
            if vault
                .read(endpoint.tenant_id, &scope)
                .await
                .map_err(|_| DeleteError::State)?
                .is_some()
            {
                let registration: crate::device::Registration =
                    load_registration(vault, endpoint.tenant_id, scope.clone())
                        .await
                        .map_err(|_| DeleteError::State)?;
                upstream
                    .revoke_controller(
                        auth,
                        endpoint.tenant_id,
                        &endpoint.account,
                        &registration.client_id,
                    )
                    .await?;
            }
        }
        EndpointKind::In => {
            let environment = match vault
                .read(endpoint.tenant_id, &scope)
                .await
                .map_err(|_| DeleteError::State)?
            {
                Some(raw) => Some(
                    serde_json::from_slice::<HostRegistration>(&raw)
                        .map_err(|_| DeleteError::State)?
                        .environment_id,
                ),
                None => {
                    store
                        .host_environment(endpoint.tenant_id, endpoint.device_id)
                        .await?
                }
            };
            if let Some(environment) = environment
                && !upstream
                    .revoke_environment_clients(
                        auth,
                        endpoint.tenant_id,
                        &endpoint.account,
                        &environment,
                    )
                    .await?
            {
                return Ok(DeleteOutcome::RevocationPending);
            }
        }
    }
    finish_local(vault, endpoint, &scope).await?;
    Ok(DeleteOutcome::Complete)
}

async fn finish_local(
    vault: &Vault,
    endpoint: &Endpoint,
    scope: &SecretScope,
) -> Result<(), DeleteError> {
    let mut tx = vault.store.pool.begin().await.map_err(StorageError::from)?;
    // Preserve shared ordinary account credentials and the tenant seed. No background
    // cleanup task survives this transaction; same-name recreation may now proceed.
    sqlx::query("DELETE FROM secrets WHERE tenant_id=? AND scope=?")
        .bind(endpoint.tenant_id.to_string())
        .bind(scope_key(scope).map_err(|_| DeleteError::State)?)
        .execute(&mut *tx)
        .await
        .map_err(StorageError::from)?;
    sqlx::query("DELETE FROM secrets WHERE tenant_id=? AND scope=?")
        .bind(endpoint.tenant_id.to_string())
        .bind(
            scope_key(&SecretScope::OutEnrollment(endpoint.device_id))
                .map_err(|_| DeleteError::State)?,
        )
        .execute(&mut *tx)
        .await
        .map_err(StorageError::from)?;
    for sql in [
        "DELETE FROM host_environments WHERE tenant_id=? AND in_id=?",
        "DELETE FROM targets WHERE tenant_id=? AND out_id=?",
        "DELETE FROM enrollment_attempts WHERE tenant_id=? AND device_id=?",
    ] {
        sqlx::query(sql)
            .bind(endpoint.tenant_id.to_string())
            .bind(endpoint.device_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?;
    }
    let changed = sqlx::query(
        "DELETE FROM endpoints WHERE tenant_id=? AND device_id=? AND lifecycle='deleting'",
    )
    .bind(endpoint.tenant_id.to_string())
    .bind(endpoint.device_id.to_string())
    .execute(&mut *tx)
    .await
    .map_err(StorageError::from)?
    .rows_affected();
    if changed != 1 {
        return Err(DeleteError::State);
    }
    tx.commit().await.map_err(StorageError::from)?;
    Ok(())
}

impl From<RuntimeError> for DeleteError {
    fn from(_: RuntimeError) -> Self {
        Self::State
    }
}

#[cfg(test)]
mod tests;
