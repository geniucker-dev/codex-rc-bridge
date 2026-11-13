//! Deployment-wide encrypted settings; tenant APIs must never expose this directly.
use crate::{
    auth::{AuthError, SecretString},
    upstream::Upstream,
    vault::{SecretScope, Vault},
};
use uuid::Uuid;
use zeroize::Zeroizing;

pub async fn load(vault: &Vault) -> Result<Option<Option<SecretString>>, AuthError> {
    let raw: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT ciphertext FROM deployment_settings WHERE id=1")
            .fetch_optional(&vault.store.pool)
            .await
            .map_err(|_| AuthError::Storage)?;
    raw.map(|raw| {
        let bytes = vault
            .key
            .open(Uuid::nil(), &SecretScope::DeploymentProxy, &raw)
            .map_err(|_| AuthError::Storage)?;
        serde_json::from_slice(&bytes).map_err(|_| AuthError::Storage)
    })
    .transpose()
}
pub async fn save(vault: &Vault, proxy: Option<&SecretString>) -> Result<(), AuthError> {
    Upstream::new(proxy.map(SecretString::expose))?;
    let raw = Zeroizing::new(serde_json::to_vec(&proxy).map_err(|_| AuthError::Storage)?);
    let encrypted = vault
        .key
        .seal(Uuid::nil(), &SecretScope::DeploymentProxy, &raw)
        .map_err(|_| AuthError::Storage)?;
    sqlx::query("INSERT INTO deployment_settings(id,ciphertext) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET ciphertext=excluded.ciphertext")
        .bind(encrypted).execute(&vault.store.pool).await.map_err(|_|AuthError::Storage)?;
    Ok(())
}
/// Existing environment settings are imported once; saved settings always win afterwards.
pub async fn bootstrap(
    vault: &Vault,
    legacy: Option<String>,
) -> Result<Option<SecretString>, AuthError> {
    if let Some(saved) = load(vault).await? {
        return Ok(saved);
    }
    let proxy = legacy
        .filter(|value| !value.is_empty())
        .map(SecretString::new);
    if proxy.is_some() {
        save(vault, proxy.as_ref()).await?;
    }
    Ok(proxy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{storage::SqliteStore, vault::EncryptionKey};
    use sqlx::sqlite::SqliteConnectOptions;
    use std::sync::Arc;
    #[tokio::test]
    async fn settings_are_encrypted_and_do_not_use_tenant_secrets() {
        let store = Arc::new(
            SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
                .await
                .unwrap(),
        );
        let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[37; 32]).unwrap());
        let proxy = SecretString::new("http://user:synthetic-password@127.0.0.1:7890".into());
        save(&vault, Some(&proxy)).await.unwrap();
        let raw: Vec<u8> = sqlx::query_scalar("SELECT ciphertext FROM deployment_settings")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert!(!raw.windows(18).any(|w| w == b"synthetic-password"));
        let wrong = Vault::new(store, EncryptionKey::from_bytes(&[38; 32]).unwrap());
        assert!(load(&wrong).await.is_err());
        assert_eq!(
            load(&vault).await.unwrap().unwrap().unwrap().expose(),
            proxy.expose()
        );
    }
}
