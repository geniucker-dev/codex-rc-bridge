use crate::{
    model::Binding,
    storage::{SqliteStore, StorageError},
};
use sqlx::Row;
use uuid::Uuid;

impl SqliteStore {
    pub(crate) async fn switch_pending(
        &self,
        tenant: Uuid,
        input: Uuid,
    ) -> Result<bool, StorageError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM chain_switches WHERE tenant_id=? AND in_id=?)",
        )
        .bind(tenant.to_string())
        .bind(input.to_string())
        .fetch_one(&self.pool)
        .await?)
    }

    pub(crate) async fn prepare_switch(&self, next: &Binding) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await?;
        let permitted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints i JOIN endpoints o ON o.tenant_id=i.tenant_id JOIN targets t ON t.tenant_id=o.tenant_id AND t.out_id=o.device_id WHERE i.tenant_id=? AND i.device_id=? AND i.kind='in' AND i.lifecycle='active' AND o.device_id=? AND o.kind='out' AND o.lifecycle='active' AND NOT EXISTS(SELECT 1 FROM host_environments h WHERE h.environment_id=t.environment_id)) AND NOT EXISTS(SELECT 1 FROM bindings WHERE tenant_id=? AND out_id=?)")
            .bind(next.tenant_id.to_string()).bind(next.in_id.to_string()).bind(next.out_id.to_string())
            .bind(next.tenant_id.to_string()).bind(next.out_id.to_string()).fetch_one(&mut *tx).await?;
        if !permitted {
            return Err(StorageError::Invalid);
        }
        let changed = sqlx::query("INSERT INTO chain_switches(tenant_id,in_id,old_out_id,new_out_id) SELECT tenant_id,in_id,out_id,? FROM bindings WHERE tenant_id=? AND in_id=?")
            .bind(next.out_id.to_string()).bind(next.tenant_id.to_string()).bind(next.in_id.to_string())
            .execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(StorageError::Invalid);
        }
        tx.commit().await?;
        Ok(())
    }

    /// Only the coordinator calls this, after all old workers have joined, or during
    /// exclusive-process startup recovery before any workers have been started.
    pub(crate) async fn commit_switch(
        &self,
        tenant: Uuid,
        input: Uuid,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT old_out_id,new_out_id FROM chain_switches WHERE tenant_id=? AND in_id=?",
        )
        .bind(tenant.to_string())
        .bind(input.to_string())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StorageError::Invalid)?;
        let old: String = row.try_get("old_out_id")?;
        let next: String = row.try_get("new_out_id")?;
        sqlx::query("DELETE FROM chain_switches WHERE tenant_id=? AND in_id=?")
            .bind(tenant.to_string())
            .bind(input.to_string())
            .execute(&mut *tx)
            .await?;
        let changed =
            sqlx::query("UPDATE bindings SET out_id=? WHERE tenant_id=? AND in_id=? AND out_id=?")
                .bind(next)
                .bind(tenant.to_string())
                .bind(input.to_string())
                .bind(old)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        if changed != 1 {
            return Err(StorageError::Invalid);
        }
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn pending_switches(&self, tenant: Uuid) -> Result<Vec<Uuid>, StorageError> {
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT in_id FROM chain_switches WHERE tenant_id=? ORDER BY in_id")
                .bind(tenant.to_string())
                .fetch_all(&self.pool)
                .await?;
        ids.into_iter()
            .map(|id| id.parse().map_err(|_| StorageError::Invalid))
            .collect()
    }

    pub(crate) async fn chain_enabled(
        &self,
        tenant: Uuid,
        input: Uuid,
    ) -> Result<bool, StorageError> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chain_settings WHERE tenant_id=? AND in_id=? AND enabled=1)")
            .bind(tenant.to_string()).bind(input.to_string()).fetch_one(&self.pool).await?)
    }

    pub(crate) async fn set_chain_enabled(
        &self,
        tenant: Uuid,
        input: Uuid,
        enabled: bool,
    ) -> Result<(), StorageError> {
        let changed = sqlx::query("INSERT INTO chain_settings(tenant_id,in_id,enabled) SELECT tenant_id,in_id,? FROM bindings WHERE tenant_id=? AND in_id=? ON CONFLICT(tenant_id,in_id) DO UPDATE SET enabled=excluded.enabled")
            .bind(enabled).bind(tenant.to_string()).bind(input.to_string()).execute(&self.pool).await?.rows_affected();
        if changed != 1 {
            return Err(StorageError::Invalid);
        }
        Ok(())
    }
}
