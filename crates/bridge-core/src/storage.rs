//! SQLite metadata foundation. Secrets are deliberately not accepted by this API.
#[cfg(test)]
mod capacity;
#[cfg(test)]
mod tests;
use crate::model::{AccountIdentity, Binding, Endpoint, EndpointKind};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("metadata operation failed")]
    Database(#[from] sqlx::Error),
    #[error(
        "database structure is unsupported and requires manual handling / 数据库结构不受支持，需要手工处理"
    )]
    UnsupportedSchema,
    #[error("stored metadata is invalid")]
    Invalid,
}

pub struct SqliteStore {
    pub(crate) pool: SqlitePool,
}
impl SqliteStore {
    pub(crate) async fn begin_device_sync(
        &self,
        tenant: Uuid,
        device: Uuid,
        now: i64,
    ) -> Result<bool, StorageError> {
        Ok(sqlx::query("UPDATE endpoints SET device_checked_at=? WHERE tenant_id=? AND device_id=? AND (device_checked_at IS NULL OR device_checked_at<=?)")
            .bind(now).bind(tenant.to_string()).bind(device.to_string()).bind(now - 30).execute(&self.pool).await?.rows_affected() != 0)
    }
    pub async fn open(options: SqliteConnectOptions) -> Result<Self, StorageError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            // One connection serializes database work. EXTRA also syncs the directory
            // after unlinking a rollback journal; remote side effects rely on commits.
            // fullfsync requests macOS's stronger disk flush (a no-op elsewhere).
            .connect_with(
                options
                    .foreign_keys(true)
                    .journal_mode(SqliteJournalMode::Delete)
                    .synchronous(SqliteSynchronous::Extra)
                    .pragma("fullfsync", "ON"),
            )
            .await?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await?;
        if version == 0 {
            let tables: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").fetch_one(&pool).await?;
            if tables != 0 {
                pool.close().await;
                return Err(StorageError::UnsupportedSchema);
            }
            sqlx::raw_sql(include_str!("schema.sql"))
                .execute(&pool)
                .await?;
        } else if version != 14 {
            pool.close().await;
            return Err(StorageError::UnsupportedSchema);
        }
        Ok(Self { pool })
    }
    pub async fn close(self) {
        self.pool.close().await;
    }
    pub async fn create_tenant(&self, tenant: Uuid) -> Result<(), StorageError> {
        sqlx::query("INSERT INTO tenants(id) VALUES (?)")
            .bind(tenant.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    /// Local deployment-operator bootstrap only; never expose this through a tenant-scoped API.
    pub async fn local_tenants(&self) -> Result<Vec<Uuid>, StorageError> {
        let values: Vec<String> = sqlx::query_scalar("SELECT id FROM tenants ORDER BY id")
            .fetch_all(&self.pool)
            .await?;
        values
            .into_iter()
            .map(|v| v.parse().map_err(|_| StorageError::Invalid))
            .collect()
    }
    pub async fn create_account(
        &self,
        tenant: Uuid,
        account: &AccountIdentity,
    ) -> Result<(), StorageError> {
        for value in [&account.workspace_id, &account.user_id] {
            crate::identity::validate_field(value).map_err(|_| StorageError::Invalid)?;
        }
        sqlx::query("INSERT INTO accounts(tenant_id, workspace_id, user_id) VALUES (?,?,?)")
            .bind(tenant.to_string())
            .bind(&account.workspace_id)
            .bind(&account.user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn sign_in_required(
        &self,
        tenant: Uuid,
        account: &AccountIdentity,
    ) -> Result<bool, StorageError> {
        Ok(sqlx::query_scalar("SELECT sign_in_required FROM accounts WHERE tenant_id=? AND workspace_id=? AND user_id=?")
            .bind(tenant.to_string()).bind(&account.workspace_id).bind(&account.user_id).fetch_one(&self.pool).await?)
    }
    pub(crate) async fn set_sign_in_required(
        &self,
        tenant: Uuid,
        account: &AccountIdentity,
        required: bool,
    ) -> Result<(), StorageError> {
        sqlx::query("UPDATE accounts SET sign_in_required=?,sign_in_revision=sign_in_revision+? WHERE tenant_id=? AND workspace_id=? AND user_id=?")
            .bind(required).bind(i64::from(required)).bind(tenant.to_string()).bind(&account.workspace_id).bind(&account.user_id).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn create_endpoint(&self, endpoint: &Endpoint) -> Result<(), StorageError> {
        crate::identity::validate_field(&endpoint.identity_name)
            .map_err(|_| StorageError::Invalid)?;
        sqlx::query("INSERT INTO endpoints(tenant_id,device_id,workspace_id,user_id,kind,identity_name,display_name) VALUES (?,?,?,?,?,?,?)")
            .bind(endpoint.tenant_id.to_string()).bind(endpoint.device_id.to_string())
            .bind(&endpoint.account.workspace_id).bind(&endpoint.account.user_id)
            .bind(endpoint.kind.as_str()).bind(&endpoint.identity_name).bind(&endpoint.display_name)
            .execute(&self.pool).await?;
        Ok(())
    }
    pub async fn endpoint(
        &self,
        tenant: Uuid,
        device: Uuid,
    ) -> Result<Option<Endpoint>, StorageError> {
        let row = sqlx::query("SELECT * FROM endpoints WHERE tenant_id=? AND device_id=?")
            .bind(tenant.to_string())
            .bind(device.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            Ok(Endpoint {
                tenant_id: tenant,
                device_id: device,
                account: AccountIdentity {
                    workspace_id: row.try_get("workspace_id")?,
                    user_id: row.try_get("user_id")?,
                },
                kind: match row.try_get::<&str, _>("kind")? {
                    "in" => EndpointKind::In,
                    "out" => EndpointKind::Out,
                    _ => return Err(StorageError::Invalid),
                },
                identity_name: row.try_get("identity_name")?,
                display_name: row.try_get("display_name")?,
            })
        })
        .transpose()
    }
    /// Configuration only; never opens sockets. Existing bindings cannot be overwritten here.
    pub async fn bind(&self, binding: &Binding) -> Result<(), StorageError> {
        sqlx::query("INSERT INTO bindings(tenant_id,in_id,out_id) VALUES (?,?,?)")
            .bind(binding.tenant_id.to_string())
            .bind(binding.in_id.to_string())
            .bind(binding.out_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn unbind(&self, tenant: Uuid, input: Uuid) -> Result<bool, StorageError> {
        Ok(
            sqlx::query("DELETE FROM bindings WHERE tenant_id=? AND in_id=?")
                .bind(tenant.to_string())
                .bind(input.to_string())
                .execute(&self.pool)
                .await?
                .rows_affected()
                == 1,
        )
    }
    pub async fn binding(
        &self,
        tenant: Uuid,
        input: Uuid,
    ) -> Result<Option<Binding>, StorageError> {
        let output: Option<String> =
            sqlx::query_scalar("SELECT out_id FROM bindings WHERE tenant_id=? AND in_id=?")
                .bind(tenant.to_string())
                .bind(input.to_string())
                .fetch_optional(&self.pool)
                .await?;
        output
            .map(|output| {
                Ok(Binding {
                    tenant_id: tenant,
                    in_id: input,
                    out_id: output.parse().map_err(|_| StorageError::Invalid)?,
                })
            })
            .transpose()
    }
    pub async fn claim_stream(
        &self,
        tenant: Uuid,
        input: Uuid,
        hash: &str,
    ) -> Result<bool, StorageError> {
        Ok(sqlx::query("INSERT INTO used_streams(tenant_id,in_id,stream_hash) VALUES(?,?,?) ON CONFLICT DO NOTHING")
            .bind(tenant.to_string()).bind(input.to_string()).bind(hash).execute(&self.pool).await?.rows_affected() == 1)
    }
    pub async fn set_target(
        &self,
        tenant: Uuid,
        output: Uuid,
        target: &crate::model::Target,
    ) -> Result<(), StorageError> {
        for value in [&target.environment_id, &target.app_server_version] {
            crate::identity::validate_field(value).map_err(|_| StorageError::Invalid)?;
        }
        let mut tx = self.pool.begin().await?;
        let permitted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints WHERE tenant_id=? AND device_id=? AND kind='out' AND lifecycle='active') AND NOT EXISTS(SELECT 1 FROM bindings WHERE tenant_id=? AND out_id=?) AND NOT EXISTS(SELECT 1 FROM host_environments WHERE environment_id=?)")
            .bind(tenant.to_string()).bind(output.to_string()).bind(tenant.to_string()).bind(output.to_string()).bind(&target.environment_id).fetch_one(&mut *tx).await?;
        if !permitted {
            return Err(StorageError::Invalid);
        }
        sqlx::query("INSERT INTO targets(tenant_id,out_id,environment_id,app_server_version) VALUES(?,?,?,?) ON CONFLICT(tenant_id,out_id) DO UPDATE SET environment_id=excluded.environment_id,app_server_version=excluded.app_server_version")
            .bind(tenant.to_string()).bind(output.to_string()).bind(&target.environment_id).bind(&target.app_server_version).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn target(
        &self,
        tenant: Uuid,
        output: Uuid,
    ) -> Result<Option<crate::model::Target>, StorageError> {
        let row = sqlx::query(
            "SELECT environment_id,app_server_version FROM targets WHERE tenant_id=? AND out_id=?",
        )
        .bind(tenant.to_string())
        .bind(output.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(crate::model::Target {
                environment_id: row.try_get("environment_id")?,
                app_server_version: row.try_get("app_server_version")?,
            })
        })
        .transpose()
    }
    pub async fn bindings(&self, tenant: Uuid) -> Result<Vec<Binding>, StorageError> {
        let rows =
            sqlx::query("SELECT in_id,out_id FROM bindings WHERE tenant_id=? ORDER BY in_id")
                .bind(tenant.to_string())
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter()
            .map(|row| {
                Ok(Binding {
                    tenant_id: tenant,
                    in_id: row
                        .try_get::<String, _>("in_id")?
                        .parse()
                        .map_err(|_| StorageError::Invalid)?,
                    out_id: row
                        .try_get::<String, _>("out_id")?
                        .parse()
                        .map_err(|_| StorageError::Invalid)?,
                })
            })
            .collect()
    }
    pub(crate) async fn host_environment(
        &self,
        tenant: Uuid,
        input: Uuid,
    ) -> Result<Option<String>, StorageError> {
        Ok(sqlx::query_scalar(
            "SELECT environment_id FROM host_environments WHERE tenant_id=? AND in_id=?",
        )
        .bind(tenant.to_string())
        .bind(input.to_string())
        .fetch_optional(&self.pool)
        .await?)
    }
    pub async fn is_local_environment(&self, environment: &str) -> Result<bool, StorageError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM host_environments WHERE environment_id=?)",
        )
        .bind(environment)
        .fetch_one(&self.pool)
        .await?)
    }
    /// Only never-authorized drafts. Remote deletion needs the lifecycle coordinator, not this method.
    pub async fn delete_draft(&self, tenant: Uuid, device: Uuid) -> Result<bool, StorageError> {
        Ok(sqlx::query(
            "DELETE FROM endpoints WHERE tenant_id=? AND device_id=? AND lifecycle='draft'",
        )
        .bind(tenant.to_string())
        .bind(device.to_string())
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }
}
