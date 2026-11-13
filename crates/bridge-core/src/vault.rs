//! Authenticated encryption binds every record to its tenant, owner and purpose.
//! The caller supplies the KEK independently of the database. Never auto-replace a lost key.
use crate::{identity::TenantSeed, model::AccountIdentity, storage::SqliteStore};
use aws_lc_rs::{aead, rand};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("secret encryption or authentication failed")]
    Crypto,
    #[error("secret storage failed")]
    Storage,
    #[error("invalid protected record")]
    Invalid,
}

// No Debug/Serialize: callers cannot accidentally log the key.
pub struct EncryptionKey(aead::LessSafeKey);
impl EncryptionKey {
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, VaultError> {
        Ok(Self(aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, bytes).map_err(|_| VaultError::Crypto)?,
        )))
    }
    pub fn seal(
        &self,
        tenant: Uuid,
        scope: &SecretScope,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, VaultError> {
        let aad = associated_data(tenant, scope)?;
        let mut nonce = [0; 12];
        rand::fill(&mut nonce).map_err(|_| VaultError::Crypto)?;
        let mut body = Zeroizing::new(plaintext.to_vec());
        self.0
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                &mut *body,
            )
            .map_err(|_| VaultError::Crypto)?;
        let mut envelope = vec![1];
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&body);
        Ok(envelope)
    }
    pub fn open(
        &self,
        tenant: Uuid,
        scope: &SecretScope,
        envelope: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        if envelope.len() < 29 || envelope[0] != 1 {
            return Err(VaultError::Invalid);
        }
        let aad = associated_data(tenant, scope)?;
        let nonce = envelope[1..13]
            .try_into()
            .map_err(|_| VaultError::Invalid)?;
        let mut body = Zeroizing::new(envelope[13..].to_vec());
        let len = self
            .0
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                &mut body,
            )
            .map_err(|_| VaultError::Crypto)?
            .len();
        body.truncate(len);
        Ok(body)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "purpose", content = "owner", rename_all = "snake_case")]
pub enum SecretScope {
    DeploymentProxy,
    TenantSeed,
    Account(AccountIdentity),
    OutRegistration(Uuid),
    OutEnrollment(Uuid),
    InRegistration(Uuid),
    OAuth(Uuid),
}
fn associated_data(tenant: Uuid, scope: &SecretScope) -> Result<Vec<u8>, VaultError> {
    serde_json::to_vec(&("codex-rc-bridge/secrets/v1", tenant, scope))
        .map_err(|_| VaultError::Invalid)
}
pub(crate) fn scope_key(scope: &SecretScope) -> Result<String, VaultError> {
    serde_json::to_string(scope).map_err(|_| VaultError::Invalid)
}

#[derive(Clone)]
pub struct Vault {
    pub(crate) store: Arc<SqliteStore>,
    pub(crate) key: Arc<EncryptionKey>,
}
impl Vault {
    pub fn new(store: Arc<SqliteStore>, key: EncryptionKey) -> Self {
        Self {
            store,
            key: Arc::new(key),
        }
    }
    /// Create a new tenant and its encrypted identity seed in one durable transaction.
    /// Existing tenants are rejected, including those whose original seed is missing.
    pub async fn create_tenant_with_seed(&self, tenant: Uuid) -> Result<(), VaultError> {
        let scope = SecretScope::TenantSeed;
        let seed = TenantSeed::generate().map_err(|_| VaultError::Crypto)?;
        let ciphertext = self.key.seal(tenant, &scope, seed.bytes())?;
        let mut tx = self
            .store
            .pool
            .begin()
            .await
            .map_err(|_| VaultError::Storage)?;
        sqlx::query("INSERT INTO tenants(id) VALUES (?)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|_| VaultError::Storage)?;
        sqlx::query("INSERT INTO secrets(tenant_id,scope,ciphertext) VALUES(?,?,?)")
            .bind(tenant.to_string())
            .bind(scope_key(&scope)?)
            .bind(ciphertext)
            .execute(&mut *tx)
            .await
            .map_err(|_| VaultError::Storage)?;
        tx.commit().await.map_err(|_| VaultError::Storage)
    }
    pub async fn read(
        &self,
        tenant: Uuid,
        scope: &SecretScope,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, VaultError> {
        let ciphertext: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT ciphertext FROM secrets WHERE tenant_id=? AND scope=?")
                .bind(tenant.to_string())
                .bind(scope_key(scope)?)
                .fetch_optional(&self.store.pool)
                .await
                .map_err(|_| VaultError::Storage)?;
        ciphertext
            .map(|bytes| self.key.open(tenant, scope, &bytes))
            .transpose()
    }
    pub async fn write(
        &self,
        tenant: Uuid,
        scope: &SecretScope,
        bytes: &[u8],
    ) -> Result<(), VaultError> {
        if matches!(
            scope,
            SecretScope::DeploymentProxy
                | SecretScope::TenantSeed
                | SecretScope::OAuth(_)
                | SecretScope::OutEnrollment(_)
        ) {
            return Err(VaultError::Invalid);
        }
        let ciphertext = self.key.seal(tenant, scope, bytes)?;
        sqlx::query("INSERT INTO secrets(tenant_id,scope,ciphertext) VALUES(?,?,?) ON CONFLICT(tenant_id,scope) DO UPDATE SET ciphertext=excluded.ciphertext")
            .bind(tenant.to_string()).bind(scope_key(scope)?).bind(ciphertext).execute(&self.store.pool).await.map_err(|_| VaultError::Storage)?;
        Ok(())
    }
    /// Save once, before remote finish. A deleting/foreign endpoint cannot receive a checkpoint.
    pub(crate) async fn save_out_enrollment(
        &self,
        tenant: Uuid,
        device: Uuid,
        registration: &crate::device::Registration,
    ) -> Result<(), VaultError> {
        let scope = SecretScope::OutEnrollment(device);
        let raw =
            Zeroizing::new(serde_json::to_vec(registration).map_err(|_| VaultError::Invalid)?);
        let encrypted = self.key.seal(tenant, &scope, &raw)?;
        let changed = sqlx::query("INSERT INTO secrets(tenant_id,scope,ciphertext) SELECT e.tenant_id,?,? FROM endpoints e JOIN enrollment_attempts a ON a.tenant_id=e.tenant_id AND a.device_id=e.device_id WHERE e.tenant_id=? AND e.device_id=? AND e.kind='out' AND e.lifecycle IN ('draft','active') ON CONFLICT DO NOTHING")
            .bind(scope_key(&scope)?).bind(encrypted).bind(tenant.to_string()).bind(device.to_string())
            .execute(&self.store.pool).await.map_err(|_|VaultError::Storage)?.rows_affected();
        if changed != 1 {
            return Err(VaultError::Invalid);
        }
        Ok(())
    }
    /// A challenge-verified original identity may restore cleanup evidence even while deleting.
    /// Never overwrite another checkpoint or an active registration, or reactivate the endpoint.
    pub(crate) async fn restore_out_enrollment(
        &self,
        tenant: Uuid,
        device: Uuid,
        registration: &crate::device::Registration,
    ) -> Result<(), VaultError> {
        let scope = SecretScope::OutEnrollment(device);
        let raw =
            Zeroizing::new(serde_json::to_vec(registration).map_err(|_| VaultError::Invalid)?);
        let encrypted = self.key.seal(tenant, &scope, &raw)?;
        let changed = sqlx::query("INSERT INTO secrets(tenant_id,scope,ciphertext) SELECT e.tenant_id,?,? FROM endpoints e JOIN enrollment_attempts a ON a.tenant_id=e.tenant_id AND a.device_id=e.device_id WHERE e.tenant_id=? AND e.device_id=? AND e.kind='out' AND e.lifecycle IN ('draft','deleting') AND NOT EXISTS(SELECT 1 FROM secrets WHERE tenant_id=e.tenant_id AND scope=?) AND NOT EXISTS(SELECT 1 FROM bindings WHERE tenant_id=e.tenant_id AND out_id=e.device_id) ON CONFLICT DO NOTHING")
            .bind(scope_key(&scope)?).bind(encrypted).bind(tenant.to_string()).bind(device.to_string())
            .bind(scope_key(&SecretScope::OutRegistration(device))?)
            .execute(&self.store.pool).await.map_err(|_| VaultError::Storage)?.rows_affected();
        if changed != 1 {
            return Err(VaultError::Invalid);
        }
        Ok(())
    }
    /// Activation and removal of the pending record commit together.
    pub async fn save_out_registration(
        &self,
        tenant: Uuid,
        device: Uuid,
        registration: &crate::device::Registration,
    ) -> Result<(), VaultError> {
        let scope = SecretScope::OutRegistration(device);
        let raw =
            Zeroizing::new(serde_json::to_vec(registration).map_err(|_| VaultError::Invalid)?);
        let encrypted = self.key.seal(tenant, &scope, &raw)?;
        let mut tx = self
            .store
            .pool
            .begin()
            .await
            .map_err(|_| VaultError::Storage)?;
        let changed = sqlx::query("UPDATE endpoints SET lifecycle='active' WHERE tenant_id=? AND device_id=? AND kind='out' AND lifecycle IN ('draft','active')")
            .bind(tenant.to_string()).bind(device.to_string()).execute(&mut *tx).await.map_err(|_| VaultError::Storage)?.rows_affected();
        if changed != 1 {
            return Err(VaultError::Invalid);
        }
        sqlx::query("INSERT INTO secrets(tenant_id,scope,ciphertext) VALUES(?,?,?) ON CONFLICT(tenant_id,scope) DO UPDATE SET ciphertext=excluded.ciphertext")
            .bind(tenant.to_string()).bind(scope_key(&scope)?).bind(encrypted).execute(&mut *tx).await.map_err(|_| VaultError::Storage)?;
        sqlx::query("DELETE FROM enrollment_attempts WHERE tenant_id=? AND device_id=?")
            .bind(tenant.to_string())
            .bind(device.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|_| VaultError::Storage)?;
        sqlx::query("DELETE FROM secrets WHERE tenant_id=? AND scope=?")
            .bind(tenant.to_string())
            .bind(scope_key(&SecretScope::OutEnrollment(device))?)
            .execute(&mut *tx)
            .await
            .map_err(|_| VaultError::Storage)?;
        tx.commit().await.map_err(|_| VaultError::Storage)
    }
    pub async fn save_host_registration(
        &self,
        tenant: Uuid,
        device: Uuid,
        registration: &crate::upstream::HostRegistration,
    ) -> Result<(), VaultError> {
        let scope = SecretScope::InRegistration(device);
        let raw =
            Zeroizing::new(serde_json::to_vec(registration).map_err(|_| VaultError::Invalid)?);
        let encrypted = self.key.seal(tenant, &scope, &raw)?;
        let mut tx = self
            .store
            .pool
            .begin()
            .await
            .map_err(|_| VaultError::Storage)?;
        // A replacement already sent before deletion must commit its cleanup identity.
        // Deletion joins that worker before reading the registration; never reactivate it.
        let changed=sqlx::query("UPDATE endpoints SET lifecycle=CASE WHEN lifecycle='draft' THEN 'active' ELSE lifecycle END WHERE tenant_id=? AND device_id=? AND kind='in' AND lifecycle IN ('draft','active','deleting')")
            .bind(tenant.to_string()).bind(device.to_string()).execute(&mut *tx).await.map_err(|_|VaultError::Storage)?.rows_affected();
        if changed != 1 {
            return Err(VaultError::Invalid);
        }
        sqlx::query("INSERT INTO secrets(tenant_id,scope,ciphertext) VALUES(?,?,?) ON CONFLICT(tenant_id,scope) DO UPDATE SET ciphertext=excluded.ciphertext")
            .bind(tenant.to_string()).bind(scope_key(&scope)?).bind(encrypted).execute(&mut *tx).await.map_err(|_|VaultError::Storage)?;
        sqlx::query("INSERT INTO host_environments(tenant_id,in_id,environment_id) VALUES(?,?,?) ON CONFLICT(tenant_id,in_id) DO UPDATE SET environment_id=excluded.environment_id")
            .bind(tenant.to_string()).bind(device.to_string()).bind(&registration.environment_id).execute(&mut *tx).await.map_err(|_|VaultError::Storage)?;
        sqlx::query("DELETE FROM enrollment_attempts WHERE tenant_id=? AND device_id=?")
            .bind(tenant.to_string())
            .bind(device.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|_| VaultError::Storage)?;
        tx.commit().await.map_err(|_| VaultError::Storage)
    }
    /// Atomic create-or-load: racing initializers cannot replace an existing identity seed.
    pub async fn tenant_seed(&self, tenant: Uuid) -> Result<TenantSeed, VaultError> {
        let scope = SecretScope::TenantSeed;
        let candidate = TenantSeed::generate().map_err(|_| VaultError::Crypto)?;
        let ciphertext = self.key.seal(tenant, &scope, candidate.bytes())?;
        sqlx::query("INSERT INTO secrets(tenant_id,scope,ciphertext) VALUES(?,?,?) ON CONFLICT(tenant_id,scope) DO NOTHING")
            .bind(tenant.to_string()).bind(scope_key(&scope)?).bind(ciphertext).execute(&self.store.pool).await.map_err(|_| VaultError::Storage)?;
        let bytes = self
            .read(tenant, &scope)
            .await?
            .ok_or(VaultError::Invalid)?;
        Ok(TenantSeed::from_bytes(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| VaultError::Invalid)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteConnectOptions;

    async fn vault() -> Vault {
        let store = Arc::new(
            SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
                .await
                .unwrap(),
        );
        Vault::new(store, EncryptionKey::from_bytes(&[43; 32]).unwrap())
    }

    #[tokio::test]
    async fn tenant_creation_rolls_back_failed_seed_write_and_can_retry() {
        let vault = vault().await;
        let tenant = Uuid::new_v4();
        sqlx::query("CREATE TRIGGER fail_seed BEFORE INSERT ON secrets BEGIN SELECT RAISE(ABORT, 'injected seed write failure'); END")
            .execute(&vault.store.pool)
            .await
            .unwrap();

        assert!(vault.create_tenant_with_seed(tenant).await.is_err());
        assert!(vault.store.local_tenants().await.unwrap().is_empty());
        assert!(
            vault
                .read(tenant, &SecretScope::TenantSeed)
                .await
                .unwrap()
                .is_none()
        );

        sqlx::query("DROP TRIGGER fail_seed")
            .execute(&vault.store.pool)
            .await
            .unwrap();
        vault.create_tenant_with_seed(tenant).await.unwrap();
        assert_eq!(vault.store.local_tenants().await.unwrap(), vec![tenant]);
        assert_eq!(
            vault
                .read(tenant, &SecretScope::TenantSeed)
                .await
                .unwrap()
                .unwrap()
                .len(),
            32
        );
    }

    #[tokio::test]
    async fn tenant_creation_never_replaces_existing_identity_or_missing_seed() {
        let vault = vault().await;
        let tenant = Uuid::new_v4();
        vault.create_tenant_with_seed(tenant).await.unwrap();
        let original = vault
            .read(tenant, &SecretScope::TenantSeed)
            .await
            .unwrap()
            .unwrap();

        assert!(vault.create_tenant_with_seed(tenant).await.is_err());
        let retained = vault
            .read(tenant, &SecretScope::TenantSeed)
            .await
            .unwrap()
            .unwrap();
        assert!(original.as_slice() == retained.as_slice());

        let missing_seed = Uuid::new_v4();
        vault.store.create_tenant(missing_seed).await.unwrap();
        assert!(vault.create_tenant_with_seed(missing_seed).await.is_err());
        assert!(
            vault
                .read(missing_seed, &SecretScope::TenantSeed)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(vault.store.local_tenants().await.unwrap().len(), 2);
    }
}
