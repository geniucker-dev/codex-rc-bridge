//! The selected computer's cached metadata is shared by on-demand sync and live probes.
use crate::{
    chains::RuntimeFactory,
    configuration::ConfigurationError,
    device::Registration,
    model::Endpoint,
    storage::{SqliteStore, StorageError},
    upstream::{ControlError, Environment},
    vault::SecretScope,
};
use uuid::Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TargetRecord {
    pub name: Option<String>,
    pub online: Option<bool>,
    pub last_seen_at: Option<i64>,
    pub error: Option<String>,
}
impl TryFrom<&Environment> for TargetRecord {
    type Error = ControlError;
    fn try_from(environment: &Environment) -> Result<Self, Self::Error> {
        let last_seen_at = environment
            .last_seen_at
            .as_deref()
            .map(|value| {
                time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                    .map(|date| date.unix_timestamp())
                    .map_err(|_| ControlError::Invalid)
            })
            .transpose()?;
        Ok(Self {
            name: environment.label(),
            online: environment.online,
            last_seen_at,
            error: None,
        })
    }
}
pub(crate) struct TargetObservation {
    pub record: Option<TargetRecord>,
    pub error: Option<&'static str>,
}
impl TargetObservation {
    pub fn from_environments(
        environments: &[Environment],
        target: &str,
    ) -> Result<Self, ControlError> {
        let selected = environments
            .iter()
            .find(|environment| environment.env_id == target);
        let record = selected.map(TargetRecord::try_from).transpose()?;
        let error = if selected.is_none() {
            Some("out_target_missing")
        } else if environments
            .iter()
            .any(|environment| environment.env_id != target)
        {
            Some("out_multiple_environments")
        } else {
            None
        };
        Ok(Self { record, error })
    }
    pub fn online(&self) -> Option<bool> {
        // Losing access still closes the live connection, without claiming the computer is offline.
        self.record
            .as_ref()
            .map(|record| record.online)
            .unwrap_or(Some(false))
    }
}

impl SqliteStore {
    pub(crate) async fn target_record(
        &self,
        tenant: Uuid,
        output: Uuid,
    ) -> Result<TargetRecord, StorageError> {
        let row = sqlx::query_as::<_, (Option<String>, Option<bool>, Option<i64>, Option<String>)>(
            "SELECT r.display_name,r.online,r.last_seen_at,r.error FROM target_records r \
             JOIN targets t ON t.tenant_id=r.tenant_id AND t.out_id=r.out_id \
             AND t.environment_id=r.environment_id WHERE r.tenant_id=? AND r.out_id=?",
        )
        .bind(tenant.to_string())
        .bind(output.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .map(|(name, online, last_seen_at, error)| TargetRecord {
                name,
                online,
                last_seen_at,
                error,
            })
            .unwrap_or_default())
    }
    pub(crate) async fn observe_target(
        &self,
        tenant: Uuid,
        output: Uuid,
        environment: &str,
        observation: &TargetObservation,
    ) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await?;
        let permitted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM targets WHERE tenant_id=? AND out_id=? AND environment_id=?) \
             AND NOT EXISTS(SELECT 1 FROM output_pairings WHERE tenant_id=? AND out_id=?)",
        ).bind(tenant.to_string()).bind(output.to_string()).bind(environment)
            .bind(tenant.to_string()).bind(output.to_string()).fetch_one(&mut *tx).await?;
        if !permitted {
            return Ok(false);
        }
        let previous = sqlx::query_as::<_, (Option<String>,Option<bool>,Option<i64>,Option<String>)>(
            "SELECT display_name,online,last_seen_at,error FROM target_records WHERE tenant_id=? AND out_id=? AND environment_id=?",
        ).bind(tenant.to_string()).bind(output.to_string()).bind(environment).fetch_optional(&mut *tx).await?
            .map(|(name,online,last_seen_at,error)| TargetRecord {name,online,last_seen_at,error}).unwrap_or_default();
        let mut next = observation
            .record
            .clone()
            .unwrap_or_else(|| previous.clone());
        next.error = observation
            .error
            .map(str::to_string)
            .or_else(|| previous.error.clone());
        if previous == next {
            return Ok(false);
        }
        write_record(&mut tx, tenant, output, environment, &next).await?;
        tx.commit().await?;
        Ok(true)
    }
}
pub(crate) async fn write_record(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant: Uuid,
    output: Uuid,
    environment: &str,
    record: &TargetRecord,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO target_records(tenant_id,out_id,environment_id,display_name,online,last_seen_at,error) \
         VALUES(?,?,?,?,?,?,?) ON CONFLICT(tenant_id,out_id) DO UPDATE SET \
         environment_id=excluded.environment_id,display_name=excluded.display_name,online=excluded.online,last_seen_at=excluded.last_seen_at,error=excluded.error",
    ).bind(tenant.to_string()).bind(output.to_string()).bind(environment).bind(&record.name)
        .bind(record.online).bind(record.last_seen_at).bind(&record.error).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn sync(
    f: &RuntimeFactory,
    endpoint: &Endpoint,
    now: i64,
) -> Result<bool, ConfigurationError> {
    let tenant = endpoint.tenant_id;
    let output = endpoint.device_id;
    let active: bool = sqlx::query_scalar(
        "SELECT lifecycle='active' FROM endpoints WHERE tenant_id=? AND device_id=?",
    )
    .bind(tenant.to_string())
    .bind(output.to_string())
    .fetch_one(&f.store.pool)
    .await
    .map_err(StorageError::from)?;
    if !active || f.store.output_pairing_pending(tenant, output).await? {
        return Ok(false);
    }
    let Some(raw) = f
        .vault
        .read(tenant, &SecretScope::OutRegistration(output))
        .await?
    else {
        return Ok(false);
    };
    let registration: Registration =
        serde_json::from_slice(&raw).map_err(|_| StorageError::Invalid)?;
    let Some(target) = f.store.target(tenant, output).await? else {
        return Ok(false);
    };
    if !f.store.begin_device_sync(tenant, output, now).await? {
        return Ok(false);
    }
    let sign_in_before = f.store.sign_in_required(tenant, &endpoint.account).await?;
    let listing = f
        .upstream
        .environments(&f.auth, tenant, &endpoint.account, &registration.client_id)
        .await
        .and_then(|environments| {
            TargetObservation::from_environments(&environments, &target.environment_id)
        });
    match listing {
        Ok(observation) => Ok(f
            .store
            .observe_target(tenant, output, &target.environment_id, &observation)
            .await?),
        Err(_) => Ok(sign_in_before != f.store.sign_in_required(tenant, &endpoint.account).await?),
    }
}
