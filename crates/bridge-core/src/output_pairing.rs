//! One OUT controls one computer. Call through the tenant actor while serving;
//! exclusive-process CLI callers may invoke this directly without any running tasks.
use crate::{
    auth::{AuthService, SecretString},
    device::Registration,
    model::{EndpointKind, Target},
    runtime::load_registration,
    storage::{SqliteStore, StorageError},
    upstream::{ControlError, Upstream},
    vault::{SecretScope, Vault},
};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum PairOutputError {
    #[error("old device access has not been cleared")]
    CleanupPending,
    #[error("pairing returned no environment")]
    NoEnvironment,
    #[error("pairing returned more than one environment")]
    MultipleEnvironments,
    #[error("output configuration is unavailable")]
    State,
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}
#[derive(Debug, Serialize)]
pub struct PairedOutput {
    pub environment_id: String,
    pub name: Option<String>,
}

pub async fn pair(
    vault: &Vault,
    auth: &AuthService<Upstream>,
    upstream: &Upstream,
    tenant: Uuid,
    output: Uuid,
    code: &SecretString,
) -> Result<PairedOutput, PairOutputError> {
    let store = &vault.store;
    let endpoint = store
        .endpoint(tenant, output)
        .await?
        .ok_or(PairOutputError::State)?;
    if endpoint.kind != EndpointKind::Out || store.endpoint_deleting(tenant, output).await? {
        return Err(PairOutputError::State);
    }
    // Reject invalid local input before any remote revocation.
    crate::upstream::normalize_pairing_code(code)?;
    let registration: Registration =
        load_registration(vault, tenant, SecretScope::OutRegistration(output))
            .await
            .map_err(|_| PairOutputError::State)?;
    store.begin_output_pairing(tenant, output).await?;
    // Only this OUT loses access. Other clients on each environment remain paired.
    upstream
        .clear_controller_environments(auth, tenant, &endpoint.account, &registration.client_id)
        .await
        .map_err(|error| match error {
            ControlError::Auth(crate::auth::AuthError::SignInRequired) => {
                PairOutputError::Control(error)
            }
            _ => PairOutputError::CleanupPending,
        })?;
    upstream
        .pair_controller(auth, tenant, &endpoint.account, &registration, code)
        .await?;
    let mut environments = upstream
        .environments(auth, tenant, &endpoint.account, &registration.client_id)
        .await?;
    // A repeated item across pages is the same environment, not a second computer.
    environments.sort_by(|a, b| a.env_id.cmp(&b.env_id));
    environments.dedup_by(|a, b| a.env_id == b.env_id);
    let environment = match environments.len() {
        0 => return Err(PairOutputError::NoEnvironment),
        1 => environments.pop().expect("one environment"),
        _ => return Err(PairOutputError::MultipleEnvironments),
    };
    let record = crate::output_sync::TargetRecord::try_from(&environment)?;
    let name = record.name.clone();
    let target = Target {
        environment_id: environment.env_id,
        app_server_version: environment
            .app_server_version
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("codex-rc-bridge/{}", env!("CARGO_PKG_VERSION"))),
    };
    store
        .finish_output_pairing(tenant, output, &target, &record)
        .await?;
    Ok(PairedOutput {
        environment_id: target.environment_id,
        name,
    })
}

impl SqliteStore {
    pub async fn output_pairing_pending(
        &self,
        tenant: Uuid,
        output: Uuid,
    ) -> Result<bool, StorageError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM output_pairings WHERE tenant_id=? AND out_id=?)",
        )
        .bind(tenant.to_string())
        .bind(output.to_string())
        .fetch_one(&self.pool)
        .await?)
    }
    async fn begin_output_pairing(&self, tenant: Uuid, output: Uuid) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO output_pairings(tenant_id,out_id) VALUES(?,?) ON CONFLICT DO NOTHING",
        )
        .bind(tenant.to_string())
        .bind(output.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    async fn finish_output_pairing(
        &self,
        tenant: Uuid,
        output: Uuid,
        target: &Target,
        record: &crate::output_sync::TargetRecord,
    ) -> Result<(), StorageError> {
        for value in [&target.environment_id, &target.app_server_version] {
            crate::identity::validate_field(value).map_err(|_| StorageError::Invalid)?;
        }
        let mut tx = self.pool.begin().await?;
        let permitted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints WHERE tenant_id=? AND device_id=? AND kind='out' AND lifecycle='active') AND NOT EXISTS(SELECT 1 FROM host_environments WHERE environment_id=?) AND EXISTS(SELECT 1 FROM output_pairings WHERE tenant_id=? AND out_id=?)")
            .bind(tenant.to_string()).bind(output.to_string()).bind(&target.environment_id)
            .bind(tenant.to_string()).bind(output.to_string()).fetch_one(&mut *tx).await?;
        if !permitted {
            return Err(StorageError::Invalid);
        }
        sqlx::query("INSERT INTO targets(tenant_id,out_id,environment_id,app_server_version) VALUES(?,?,?,?) ON CONFLICT(tenant_id,out_id) DO UPDATE SET environment_id=excluded.environment_id,app_server_version=excluded.app_server_version")
            .bind(tenant.to_string()).bind(output.to_string()).bind(&target.environment_id).bind(&target.app_server_version).execute(&mut *tx).await?;
        crate::output_sync::write_record(&mut tx, tenant, output, &target.environment_id, record)
            .await?;
        sqlx::query(
            "UPDATE endpoints SET device_checked_at=NULL WHERE tenant_id=? AND device_id=?",
        )
        .bind(tenant.to_string())
        .bind(output.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM output_pairings WHERE tenant_id=? AND out_id=?")
            .bind(tenant.to_string())
            .bind(output.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
