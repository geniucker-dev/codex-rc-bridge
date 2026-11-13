//! On-demand reconciliation; snapshots keep using local data while the request runs.
use crate::{
    chains::RuntimeFactory,
    configuration::ConfigurationError,
    input_pairing::{PairedDevice, save_device},
    model::Endpoint,
    runtime::load_registration,
    storage::StorageError,
    upstream::HostRegistration,
    vault::SecretScope,
};

pub(crate) async fn sync(
    f: &RuntimeFactory,
    endpoint: &Endpoint,
    now: i64,
) -> Result<bool, ConfigurationError> {
    let tenant = endpoint.tenant_id;
    let input = endpoint.device_id;
    let host: HostRegistration =
        load_registration(&f.vault, tenant, SecretScope::InRegistration(input)).await?;
    if !f.store.begin_device_sync(tenant, input, now).await? {
        return Ok(false);
    }
    let sign_in_before = f.store.sign_in_required(tenant, &endpoint.account).await?;
    let clients = match f
        .upstream
        .environment_client_details(&f.auth, tenant, &endpoint.account, &host.environment_id)
        .await
    {
        Ok(clients) => clients,
        Err(_) => {
            // OAuth rejection may have changed the account marker. Keep device data intact.
            return Ok(sign_in_before != f.store.sign_in_required(tenant, &endpoint.account).await?);
        }
    };
    if clients.len() > 1 {
        let changed = sqlx::query(
            "INSERT INTO input_pairings(tenant_id,in_id,environment_id,pending,error) \
             VALUES(?,?,?,0,'in_multiple_devices') ON CONFLICT(tenant_id,in_id) \
             DO UPDATE SET error=excluded.error WHERE error IS NOT excluded.error",
        )
        .bind(tenant.to_string())
        .bind(input.to_string())
        .bind(&host.environment_id)
        .execute(&f.store.pool)
        .await
        .map_err(StorageError::from)?
        .rows_affected();
        return Ok(changed != 0);
    }
    let paired = match clients
        .into_iter()
        .next()
        .map(PairedDevice::try_from)
        .transpose()
    {
        Ok(device) => device,
        // An invalid listing is a read failure, not evidence that access disappeared.
        Err(_) => return Ok(false),
    };
    let raw: Option<String> =
        sqlx::query_scalar("SELECT metadata FROM input_devices WHERE tenant_id=? AND in_id=?")
            .bind(tenant.to_string())
            .bind(input.to_string())
            .fetch_optional(&f.store.pool)
            .await
            .map_err(StorageError::from)?;
    let previous: Option<PairedDevice> = raw
        .map(|raw| serde_json::from_str(&raw).map_err(|_| StorageError::Invalid))
        .transpose()?;
    if previous == paired {
        return Ok(false);
    }
    let mut tx = f.store.pool.begin().await.map_err(StorageError::from)?;
    if let Some(paired) = paired {
        save_device(&mut tx, tenant, input, &paired).await?;
    } else {
        sqlx::query("DELETE FROM input_devices WHERE tenant_id=? AND in_id=?")
            .bind(tenant.to_string())
            .bind(input.to_string())
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?;
    }
    tx.commit().await.map_err(StorageError::from)?;
    Ok(true)
}
