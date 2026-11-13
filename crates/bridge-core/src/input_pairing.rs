//! One device per IN. Pairing attempts and repair errors survive restart; codes are never stored.
use crate::{
    chains::RuntimeFactory,
    model::Endpoint,
    storage::{SqliteStore, StorageError},
    upstream::unix_time,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PairedDevice {
    pub client_id: String,
    pub name: Option<String>,
    pub platform: Option<String>,
    pub device_model: Option<String>,
    pub device_type: Option<String>,
    pub last_seen_at: Option<i64>,
}
impl TryFrom<crate::upstream::PairedClient> for PairedDevice {
    type Error = crate::upstream::ControlError;
    fn try_from(client: crate::upstream::PairedClient) -> Result<Self, Self::Error> {
        let name = [&client.display_name, &client.device_model, &client.platform]
            .into_iter()
            .flatten()
            .find(|name| !name.trim().is_empty())
            .cloned();
        let last_seen_at = client
            .last_seen_at
            .as_deref()
            .map(|value| {
                time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                    .map(|date| date.unix_timestamp())
                    .map_err(|_| crate::upstream::ControlError::Invalid)
            })
            .transpose()?;
        Ok(Self {
            client_id: client.client_id,
            name,
            platform: client.platform,
            device_model: client.device_model,
            device_type: client.device_type,
            last_seen_at,
        })
    }
}
#[derive(Debug, thiserror::Error)]
pub enum PairInputError {
    #[error("input pairing cleanup is pending")]
    CleanupPending,
    #[error("multiple devices redeemed an input pairing code")]
    MultipleDevices,
}
impl PairInputError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CleanupPending => "in_pairing_cleanup_pending",
            Self::MultipleDevices => "in_pairing_multiple_devices",
        }
    }
}
#[derive(Default)]
pub(crate) struct PairingState {
    pub pending: bool,
    pub device: Option<PairedDevice>,
    pub error: Option<String>,
}
/// Read all pairing fields together so completion cannot split a snapshot across states.
pub(crate) async fn snapshot(
    store: &SqliteStore,
    tenant: Uuid,
    id: Uuid,
) -> Result<PairingState, StorageError> {
    let (pending, error, raw): (bool, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT COALESCE(p.pending,0),p.error,d.metadata FROM endpoints e \
         LEFT JOIN input_pairings p ON p.tenant_id=e.tenant_id AND p.in_id=e.device_id \
         LEFT JOIN input_devices d ON d.tenant_id=e.tenant_id AND d.in_id=e.device_id \
         WHERE e.tenant_id=? AND e.device_id=?",
    )
    .bind(tenant.to_string())
    .bind(id.to_string())
    .fetch_one(&store.pool)
    .await?;
    Ok(PairingState {
        pending,
        device: if pending {
            None
        } else {
            raw.map(|raw| serde_json::from_str(&raw).map_err(|_| StorageError::Invalid))
                .transpose()?
        },
        error,
    })
}
pub async fn pending(store: &SqliteStore, tenant: Uuid, id: Uuid) -> Result<bool, StorageError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM input_pairings WHERE tenant_id=? AND in_id=? AND pending=1)",
    )
    .bind(tenant.to_string())
    .bind(id.to_string())
    .fetch_one(&store.pool)
    .await?)
}
pub async fn error(
    store: &SqliteStore,
    tenant: Uuid,
    id: Uuid,
) -> Result<Option<String>, StorageError> {
    Ok(sqlx::query_scalar::<_, Option<String>>(
        "SELECT error FROM input_pairings WHERE tenant_id=? AND in_id=?",
    )
    .bind(tenant.to_string())
    .bind(id.to_string())
    .fetch_optional(&store.pool)
    .await?
    .flatten())
}
pub async fn device(
    store: &SqliteStore,
    tenant: Uuid,
    id: Uuid,
) -> Result<Option<PairedDevice>, StorageError> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT d.metadata FROM input_devices d WHERE tenant_id=? AND in_id=? \
         AND NOT EXISTS(SELECT 1 FROM input_pairings p \
         WHERE p.tenant_id=d.tenant_id AND p.in_id=d.in_id AND pending=1)",
    )
    .bind(tenant.to_string())
    .bind(id.to_string())
    .fetch_optional(&store.pool)
    .await?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(|_| StorageError::Invalid))
        .transpose()
}
pub async fn begin(
    f: &RuntimeFactory,
    endpoint: &Endpoint,
    env: &str,
) -> Result<(), crate::configuration::ConfigurationError> {
    sqlx::query(
        "INSERT INTO input_pairings(tenant_id,in_id,environment_id) VALUES(?,?,?) \
         ON CONFLICT(tenant_id,in_id) DO UPDATE SET \
         environment_id=excluded.environment_id,pending=1,expires_at=NULL,error=NULL",
    )
    .bind(endpoint.tenant_id.to_string())
    .bind(endpoint.device_id.to_string())
    .bind(env)
    .execute(&f.store.pool)
    .await
    .map_err(StorageError::from)?;
    if !matches!(
        f.upstream
            .revoke_environment_clients(&f.auth, endpoint.tenant_id, &endpoint.account, env)
            .await,
        Ok(true)
    ) {
        mark_error(
            &f.store,
            endpoint.tenant_id,
            endpoint.device_id,
            PairInputError::CleanupPending.code(),
        )
        .await?;
        return Err(PairInputError::CleanupPending.into());
    }
    sqlx::query("DELETE FROM input_devices WHERE tenant_id=? AND in_id=?")
        .bind(endpoint.tenant_id.to_string())
        .bind(endpoint.device_id.to_string())
        .execute(&f.store.pool)
        .await
        .map_err(StorageError::from)?;
    Ok(())
}
async fn mark_error(
    store: &SqliteStore,
    tenant: Uuid,
    id: Uuid,
    error: &str,
) -> Result<(), StorageError> {
    sqlx::query("UPDATE input_pairings SET pending=1,error=? WHERE tenant_id=? AND in_id=?")
        .bind(error)
        .bind(tenant.to_string())
        .bind(id.to_string())
        .execute(&store.pool)
        .await?;
    Ok(())
}
pub async fn issued(
    store: &SqliteStore,
    tenant: Uuid,
    id: Uuid,
    expires: i64,
) -> Result<(), StorageError> {
    sqlx::query("UPDATE input_pairings SET expires_at=? WHERE tenant_id=? AND in_id=?")
        .bind(expires)
        .bind(tenant.to_string())
        .bind(id.to_string())
        .execute(&store.pool)
        .await?;
    Ok(())
}
pub async fn cancel(store: &SqliteStore, tenant: Uuid, id: Uuid) -> Result<(), StorageError> {
    sqlx::query("UPDATE input_pairings SET expires_at=NULL WHERE tenant_id=? AND in_id=?")
        .bind(tenant.to_string())
        .bind(id.to_string())
        .execute(&store.pool)
        .await?;
    Ok(())
}
async fn observe(
    f: &RuntimeFactory,
    endpoint: &Endpoint,
    env: &str,
    changes: &tokio::sync::watch::Sender<()>,
) -> Result<bool, crate::configuration::ConfigurationError> {
    let clients = f
        .upstream
        .environment_client_details(&f.auth, endpoint.tenant_id, &endpoint.account, env)
        .await?;
    let conflict = error(&f.store, endpoint.tenant_id, endpoint.device_id)
        .await?
        .as_deref()
        == Some(PairInputError::MultipleDevices.code());
    if clients.len() > 1 || conflict {
        if !conflict {
            mark_error(
                &f.store,
                endpoint.tenant_id,
                endpoint.device_id,
                PairInputError::MultipleDevices.code(),
            )
            .await?;
            changes.send_replace(());
        }
        if f.upstream
            .revoke_environment_clients(&f.auth, endpoint.tenant_id, &endpoint.account, env)
            .await?
        {
            sqlx::query("DELETE FROM input_devices WHERE tenant_id=? AND in_id=?")
                .bind(endpoint.tenant_id.to_string())
                .bind(endpoint.device_id.to_string())
                .execute(&f.store.pool)
                .await
                .map_err(StorageError::from)?;
            changes.send_replace(());
            return Ok(true);
        }
    } else if let Some(client) = clients.into_iter().next() {
        let paired = PairedDevice::try_from(client)?;
        if device(&f.store, endpoint.tenant_id, endpoint.device_id)
            .await?
            .as_ref()
            == Some(&paired)
        {
            return Ok(false);
        }
        let mut tx = f.store.pool.begin().await.map_err(StorageError::from)?;
        save_device(&mut tx, endpoint.tenant_id, endpoint.device_id, &paired).await?;
        sqlx::query("UPDATE input_pairings SET pending=0,error=NULL WHERE tenant_id=? AND in_id=?")
            .bind(endpoint.tenant_id.to_string())
            .bind(endpoint.device_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?;
        tx.commit().await.map_err(StorageError::from)?;
        changes.send_replace(());
    }
    Ok(false)
}
pub(crate) async fn save_device(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant: Uuid,
    id: Uuid,
    device: &PairedDevice,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO input_devices(tenant_id,in_id,metadata) VALUES(?,?,?) \
         ON CONFLICT(tenant_id,in_id) DO UPDATE SET metadata=excluded.metadata",
    )
    .bind(tenant.to_string())
    .bind(id.to_string())
    .bind(serde_json::to_string(device).map_err(|_| StorageError::Invalid)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
pub async fn follow(
    f: RuntimeFactory,
    tenant: Uuid,
    id: Uuid,
    expires: i64,
    stop: CancellationToken,
    changes: tokio::sync::watch::Sender<()>,
) {
    let setup = async {
        let endpoint = f
            .store
            .endpoint(tenant, id)
            .await?
            .ok_or(StorageError::Invalid)?;
        let env: String = sqlx::query_scalar(
            "SELECT environment_id FROM input_pairings WHERE tenant_id=? AND in_id=?",
        )
        .bind(tenant.to_string())
        .bind(id.to_string())
        .fetch_one(&f.store.pool)
        .await?;
        Ok::<_, StorageError>((endpoint, env))
    }
    .await;
    if let Ok((endpoint, env)) = setup {
        while !stop.is_cancelled() && unix_time() < expires {
            let result = observe(&f, &endpoint, &env, &changes).await;
            // Account failures may have persisted a sign-in marker during the request.
            if result.is_err() {
                changes.send_replace(());
            }
            if matches!(result, Ok(true)) {
                break;
            }
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }
        }
    }
    let _ = cancel(&f.store, tenant, id).await;
    changes.send_replace(());
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    #[test]
    fn device_name_uses_first_nonblank_label_without_exposing_client_id_as_name() {
        for (display, model, platform, expected) in [
            (Some("Renamed"), Some("iPad"), Some("iOS"), Some("Renamed")),
            (Some("  "), Some("iPad"), Some("iOS"), Some("iPad")),
            (Some(""), Some("  "), Some("iOS"), Some("iOS")),
            (None, None, None, None),
            (Some(""), Some(""), Some(" "), None),
        ] {
            let paired = PairedDevice::try_from(crate::upstream::PairedClient {
                client_id: "synthetic-id".into(),
                display_name: display.map(str::to_string),
                platform: platform.map(str::to_string),
                device_model: model.map(str::to_string),
                device_type: None,
                last_seen_at: None,
            })
            .unwrap();
            assert_eq!(paired.name.as_deref(), expected);
            assert_eq!(paired.last_seen_at, None);
            assert_eq!(paired.device_type, None);
        }
    }
}
