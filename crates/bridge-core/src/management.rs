//! Management passwords/sessions are separate from upstream OAuth and derived device identity.
#[cfg(test)]
mod tests;
use crate::{auth::SecretString, device::digest_url, storage::SqliteStore};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sqlx::Row;
use std::{collections::VecDeque, sync::Arc};
use tokio::{
    sync::{Mutex, Semaphore},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const SESSION_SECONDS: i64 = 12 * 60 * 60;
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

#[derive(Debug, thiserror::Error)]
pub enum ManagementError {
    #[error("management credentials or session are invalid")]
    Unauthorized,
    #[error("too many login attempts; try again later")]
    Limited,
    #[error("invalid username or password")]
    Invalid,
    #[error("management state operation failed")]
    Storage,
    #[error("invalid username")]
    InvalidUsername,
    #[error("invalid password")]
    InvalidPassword,
    #[error("username exists")]
    UsernameExists,
    #[error("user not found")]
    UserNotFound,
    #[error("target is administrator")]
    TargetIsAdmin,
    #[error("current password incorrect")]
    CurrentPasswordIncorrect,
}
#[derive(Clone, Serialize)]
pub struct SessionIdentity {
    pub deployment_admin: bool,
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub username: String,
    pub expires_at: i64,
}
// No Debug: the raw cookie is a credential. The browser gets it only as HttpOnly Set-Cookie.
pub struct Login {
    pub token: SecretString,
    pub identity: SessionIdentity,
}
#[derive(Serialize)]
pub struct UserInfo {
    pub id: Uuid,
    pub username: String,
    pub deployment_admin: bool,
    pub locked: bool,
    pub current: bool,
}
#[cfg(test)]
struct VerificationGate {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}
pub struct ManagementAuth {
    store: Arc<SqliteStore>,
    workers: Arc<Semaphore>,
    attempts: Mutex<VecDeque<Instant>>,
    #[cfg(test)]
    verification_gate: Mutex<Option<VerificationGate>>,
}
impl ManagementAuth {
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self {
            store,
            workers: Arc::new(Semaphore::new(2)),
            attempts: Mutex::new(VecDeque::new()),
            #[cfg(test)]
            verification_gate: Mutex::new(None),
        }
    }
    #[cfg(test)]
    async fn pause_verification(&self) {
        let gate = self.verification_gate.lock().await.take();
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            let _ = gate.release.await;
        }
    }
    async fn admin_hash(&self, password: SecretString) -> Result<String, ManagementError> {
        let permit = self
            .workers
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ManagementError::Storage)?;
        hash_password_with_permit(password, Some(permit)).await
    }
    async fn web_hash(&self, password: SecretString) -> Result<String, ManagementError> {
        let permit = self
            .workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| ManagementError::Limited)?;
        hash_password_with_permit(password, Some(permit)).await
    }
    pub async fn users(&self, caller: Uuid, now: i64) -> Result<Vec<UserInfo>, ManagementError> {
        let rows = sqlx::query("SELECT id,username,deployment_admin,locked_until FROM management_users ORDER BY username")
            .fetch_all(&self.store.pool).await.map_err(|_| ManagementError::Storage)?;
        rows.into_iter()
            .map(|r| {
                let id: Uuid = r
                    .get::<String, _>("id")
                    .parse()
                    .map_err(|_| ManagementError::Storage)?;
                Ok(UserInfo {
                    id,
                    username: r.get("username"),
                    deployment_admin: r.get("deployment_admin"),
                    locked: r.get::<i64, _>("locked_until") > now,
                    current: id == caller,
                })
            })
            .collect()
    }
    pub async fn add_web_user(
        &self,
        vault: &crate::vault::Vault,
        username: &str,
        password: SecretString,
    ) -> Result<(Uuid, Uuid), ManagementError> {
        validate_username(username)?;
        validate_password(&password)?;
        let hash = self.admin_hash(password).await?;
        let tenant = Uuid::new_v4();
        let id = Uuid::new_v4();
        let scope = crate::vault::SecretScope::TenantSeed;
        let seed = crate::identity::TenantSeed::generate().map_err(|_| ManagementError::Storage)?;
        let ciphertext = vault
            .key
            .seal(tenant, &scope, seed.bytes())
            .map_err(|_| ManagementError::Storage)?;
        let mut tx = self
            .store
            .pool
            .begin()
            .await
            .map_err(|_| ManagementError::Storage)?;
        if sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM management_users WHERE username=?)",
        )
        .bind(username)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| ManagementError::Storage)?
        {
            return Err(ManagementError::UsernameExists);
        }
        sqlx::query("INSERT INTO tenants(id) VALUES(?)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|_| ManagementError::Storage)?;
        sqlx::query("INSERT INTO secrets(tenant_id,scope,ciphertext) VALUES(?,?,?)")
            .bind(tenant.to_string())
            .bind(crate::vault::scope_key(&scope).map_err(|_| ManagementError::Storage)?)
            .bind(ciphertext)
            .execute(&mut *tx)
            .await
            .map_err(|_| ManagementError::Storage)?;
        sqlx::query(
            "INSERT INTO management_users(id,tenant_id,username,password_hash) VALUES(?,?,?,?)",
        )
        .bind(id.to_string())
        .bind(tenant.to_string())
        .bind(username)
        .bind(hash)
        .execute(&mut *tx)
        .await
        .map_err(|_| ManagementError::Storage)?;
        tx.commit().await.map_err(|_| ManagementError::Storage)?;
        Ok((id, tenant))
    }
    pub async fn non_admin_tenant(&self, id: Uuid) -> Result<Uuid, ManagementError> {
        let row = sqlx::query("SELECT tenant_id,deployment_admin FROM management_users WHERE id=?")
            .bind(id.to_string())
            .fetch_optional(&self.store.pool)
            .await
            .map_err(|_| ManagementError::Storage)?
            .ok_or(ManagementError::UserNotFound)?;
        if row.get::<bool, _>("deployment_admin") {
            return Err(ManagementError::TargetIsAdmin);
        }
        row.get::<String, _>("tenant_id")
            .parse()
            .map_err(|_| ManagementError::Storage)
    }
    pub async fn reset_web_password(
        &self,
        id: Uuid,
        password: SecretString,
    ) -> Result<(), ManagementError> {
        self.non_admin_tenant(id).await?;
        validate_password(&password)?;
        let hash = self.admin_hash(password).await?;
        let mut tx = self
            .store
            .pool
            .begin()
            .await
            .map_err(|_| ManagementError::Storage)?;
        let n = sqlx::query("UPDATE management_users SET password_hash=?,failures=0,locked_until=0 WHERE id=? AND deployment_admin=0").bind(hash).bind(id.to_string()).execute(&mut *tx).await.map_err(|_| ManagementError::Storage)?.rows_affected();
        if n != 1 {
            return Err(ManagementError::UserNotFound);
        }
        sqlx::query("DELETE FROM management_sessions WHERE user_id=?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|_| ManagementError::Storage)?;
        tx.commit().await.map_err(|_| ManagementError::Storage)
    }
    pub async fn change_password(
        &self,
        token: &str,
        current: SecretString,
        password: SecretString,
        now: i64,
    ) -> Result<(), ManagementError> {
        let identity = self.session(token, now).await?;
        validate_password(&password)?;
        let row = sqlx::query("SELECT password_hash,locked_until FROM management_users WHERE id=?")
            .bind(identity.user_id.to_string())
            .fetch_optional(&self.store.pool)
            .await
            .map_err(|_| ManagementError::Storage)?
            .ok_or(ManagementError::Unauthorized)?;
        if row.get::<i64, _>("locked_until") > now {
            return Err(ManagementError::Limited);
        }
        let old: String = row.get("password_hash");
        #[cfg(test)]
        self.pause_verification().await;
        let encoded = old.clone();
        let permit = self
            .workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| ManagementError::Limited)?;
        let correct = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            PasswordHash::new(&encoded).is_ok_and(|parsed| {
                Argon2::default()
                    .verify_password(current.expose().as_bytes(), &parsed)
                    .is_ok()
            })
        })
        .await
        .map_err(|_| ManagementError::Storage)?;
        if !correct {
            sqlx::query("UPDATE management_users SET failures=failures+1,locked_until=CASE WHEN failures+1>=5 THEN ? ELSE locked_until END WHERE id=? AND password_hash=?").bind(now+60).bind(identity.user_id.to_string()).bind(old).execute(&self.store.pool).await.map_err(|_| ManagementError::Storage)?;
            return Err(ManagementError::CurrentPasswordIncorrect);
        }
        let hash = self.web_hash(password).await?;
        self.commit_password_change(token, identity.user_id, &old, &hash, now)
            .await
    }
    async fn commit_password_change(
        &self,
        token: &str,
        id: Uuid,
        old: &str,
        hash: &str,
        now: i64,
    ) -> Result<(), ManagementError> {
        let mut tx = self
            .store
            .pool
            .begin()
            .await
            .map_err(|_| ManagementError::Storage)?;
        let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM management_sessions WHERE token_hash=? AND user_id=? AND expires_at>?)").bind(digest_url(token.as_bytes())).bind(id.to_string()).bind(now).fetch_one(&mut *tx).await.map_err(|_| ManagementError::Storage)?;
        if !valid {
            return Err(ManagementError::Unauthorized);
        }
        let n = sqlx::query("UPDATE management_users SET password_hash=?,failures=0,locked_until=0 WHERE id=? AND password_hash=? AND locked_until<=?").bind(hash).bind(id.to_string()).bind(old).bind(now).execute(&mut *tx).await.map_err(|_| ManagementError::Storage)?.rows_affected();
        if n != 1 {
            return Err(ManagementError::CurrentPasswordIncorrect);
        }
        sqlx::query("DELETE FROM management_sessions WHERE user_id=? AND token_hash<>?")
            .bind(id.to_string())
            .bind(digest_url(token.as_bytes()))
            .execute(&mut *tx)
            .await
            .map_err(|_| ManagementError::Storage)?;
        tx.commit().await.map_err(|_| ManagementError::Storage)
    }
    /// The caller has removed and joined the tenant coordinator before entering here.
    pub async fn delete_web_user(&self, id: Uuid) -> Result<(), ManagementError> {
        let mut tx = self
            .store
            .pool
            .begin()
            .await
            .map_err(|_| ManagementError::Storage)?;
        let row = sqlx::query("SELECT tenant_id,deployment_admin FROM management_users WHERE id=?")
            .bind(id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ManagementError::Storage)?
            .ok_or(ManagementError::UserNotFound)?;
        if row.get::<bool, _>("deployment_admin") {
            return Err(ManagementError::TargetIsAdmin);
        }
        let tenant: String = row.get("tenant_id");
        // Switch reservations must disappear before deleting bindings or targets.
        for table in [
            "chain_switches",
            "chain_settings",
            "bindings",
            "target_records",
            "output_pairings",
            "input_devices",
            "input_pairings",
            "enrollment_attempts",
            "targets",
            "host_environments",
            "used_streams",
            "oauth_transactions",
            "secrets",
            "endpoints",
            "accounts",
        ] {
            sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id=?"))
                .bind(&tenant)
                .execute(&mut *tx)
                .await
                .map_err(|_| ManagementError::Storage)?;
        }
        sqlx::query("DELETE FROM management_users WHERE id=?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|_| ManagementError::Storage)?;
        sqlx::query("DELETE FROM tenants WHERE id=?")
            .bind(tenant)
            .execute(&mut *tx)
            .await
            .map_err(|_| ManagementError::Storage)?;
        tx.commit().await.map_err(|_| ManagementError::Storage)
    }
    pub async fn has_users(&self) -> Result<bool, ManagementError> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM management_users)")
            .fetch_one(&self.store.pool)
            .await
            .map_err(|_| ManagementError::Storage)
    }
    /// Local operator only: account creation is never an unauthenticated HTTP route.
    pub async fn create_user(
        &self,
        tenant: Uuid,
        username: &str,
        password: SecretString,
    ) -> Result<(), ManagementError> {
        validate(username, &password)?;
        let hash = hash_password(password).await?;
        sqlx::query(
            "INSERT INTO management_users(id,tenant_id,username,password_hash,deployment_admin) VALUES(?,?,?,?,NOT EXISTS(SELECT 1 FROM management_users))",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(tenant.to_string())
        .bind(username)
        .bind(hash)
        .execute(&self.store.pool)
        .await
        .map_err(|_| ManagementError::Storage)?;
        Ok(())
    }
    /// Local operator only; never exposed through tenant management APIs.
    pub async fn grant_deployment_admin(
        &self,
        tenant: Uuid,
        username: &str,
    ) -> Result<(), ManagementError> {
        let n = sqlx::query(
            "UPDATE management_users SET deployment_admin=1 WHERE tenant_id=? AND username=?",
        )
        .bind(tenant.to_string())
        .bind(username)
        .execute(&self.store.pool)
        .await
        .map_err(|_| ManagementError::Storage)?
        .rows_affected();
        if n != 1 {
            return Err(ManagementError::Invalid);
        }
        Ok(())
    }
    /// Local reset also invalidates every existing session atomically.
    pub async fn reset_password(
        &self,
        tenant: Uuid,
        username: &str,
        password: SecretString,
    ) -> Result<(), ManagementError> {
        validate(username, &password)?;
        let hash = hash_password(password).await?;
        let mut tx = self
            .store
            .pool
            .begin()
            .await
            .map_err(|_| ManagementError::Storage)?;
        let id: Option<String> =
            sqlx::query_scalar("SELECT id FROM management_users WHERE tenant_id=? AND username=?")
                .bind(tenant.to_string())
                .bind(username)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| ManagementError::Storage)?;
        let id = id.ok_or(ManagementError::Unauthorized)?;
        sqlx::query(
            "UPDATE management_users SET password_hash=?,failures=0,locked_until=0 WHERE id=?",
        )
        .bind(hash)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(|_| ManagementError::Storage)?;
        sqlx::query("DELETE FROM management_sessions WHERE user_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|_| ManagementError::Storage)?;
        tx.commit().await.map_err(|_| ManagementError::Storage)
    }
    pub async fn login(
        &self,
        username: &str,
        password: SecretString,
        now: i64,
    ) -> Result<Login, ManagementError> {
        if validate(username, &password).is_err() {
            return Err(ManagementError::Unauthorized);
        }
        {
            let mut attempts = self.attempts.lock().await;
            let instant = Instant::now();
            while attempts
                .front()
                .is_some_and(|t| instant.duration_since(*t) >= Duration::from_secs(60))
            {
                attempts.pop_front();
            }
            if attempts.len() >= 30 {
                return Err(ManagementError::Limited);
            }
            attempts.push_back(instant);
        }
        let permit = self
            .workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| ManagementError::Limited)?;
        let row = sqlx::query(
            "SELECT id,tenant_id,password_hash,locked_until,deployment_admin FROM management_users WHERE username=?",
        )
        .bind(username)
        .fetch_optional(&self.store.pool)
        .await
        .map_err(|_| ManagementError::Storage)?;
        let hash: String = match &row {
            Some(row) => row
                .try_get("password_hash")
                .map_err(|_| ManagementError::Storage)?,
            None => DUMMY_HASH.into(),
        };
        #[cfg(test)]
        self.pause_verification().await;
        let encoded = hash.clone();
        let correct = tokio::task::spawn_blocking(move || {
            let _permit = permit; // Retain capacity even if the HTTP caller disappears during hashing.
            PasswordHash::new(&encoded).is_ok_and(|parsed| {
                Argon2::default()
                    .verify_password(password.expose().as_bytes(), &parsed)
                    .is_ok()
            })
        })
        .await
        .map_err(|_| ManagementError::Storage)?;
        let row = row.ok_or(ManagementError::Unauthorized)?;
        let id: String = row.try_get("id").map_err(|_| ManagementError::Storage)?;
        let tenant: String = row
            .try_get("tenant_id")
            .map_err(|_| ManagementError::Storage)?;
        let locked: i64 = row
            .try_get("locked_until")
            .map_err(|_| ManagementError::Storage)?;
        if locked > now {
            return Err(ManagementError::Unauthorized);
        }
        if !correct {
            sqlx::query("UPDATE management_users SET failures=failures+1,locked_until=CASE WHEN failures+1>=5 THEN ? ELSE locked_until END WHERE id=? AND password_hash=?")
                .bind(now+60).bind(&id).bind(&hash).execute(&self.store.pool).await.map_err(|_|ManagementError::Storage)?;
            return Err(ManagementError::Unauthorized);
        }
        let mut raw = zeroize::Zeroizing::new([0u8; 32]);
        crate::fill_random(raw.as_mut()).map_err(|_| ManagementError::Storage)?;
        let token = SecretString::new(URL_SAFE_NO_PAD.encode(raw.as_ref()));
        let expires_at = now
            .checked_add(SESSION_SECONDS)
            .ok_or(ManagementError::Storage)?;
        let mut tx = self
            .store
            .pool
            .begin()
            .await
            .map_err(|_| ManagementError::Storage)?;
        // A concurrent password reset or lockout must invalidate the precomputed verification.
        let updated=sqlx::query("UPDATE management_users SET failures=0,locked_until=0 WHERE id=? AND password_hash=? AND locked_until<=?")
            .bind(&id).bind(hash).bind(now).execute(&mut *tx).await.map_err(|_|ManagementError::Storage)?.rows_affected();
        if updated != 1 {
            return Err(ManagementError::Unauthorized);
        }
        sqlx::query("DELETE FROM management_sessions WHERE expires_at<=?")
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|_| ManagementError::Storage)?;
        sqlx::query("DELETE FROM management_sessions WHERE token_hash IN (SELECT token_hash FROM management_sessions WHERE user_id=? ORDER BY created_at DESC,rowid DESC LIMIT -1 OFFSET 9)")
            .bind(&id).execute(&mut *tx).await.map_err(|_|ManagementError::Storage)?;
        sqlx::query("INSERT INTO management_sessions(token_hash,user_id,expires_at,created_at) VALUES(?,?,?,?)")
            .bind(digest_url(token.expose().as_bytes())).bind(&id).bind(expires_at).bind(now).execute(&mut *tx).await.map_err(|_|ManagementError::Storage)?;
        tx.commit().await.map_err(|_| ManagementError::Storage)?;
        Ok(Login {
            token,
            identity: SessionIdentity {
                deployment_admin: row
                    .try_get("deployment_admin")
                    .map_err(|_| ManagementError::Storage)?,
                user_id: id.parse().map_err(|_| ManagementError::Storage)?,
                tenant_id: tenant.parse().map_err(|_| ManagementError::Storage)?,
                username: username.into(),
                expires_at,
            },
        })
    }
    pub async fn session(&self, token: &str, now: i64) -> Result<SessionIdentity, ManagementError> {
        validate_token(token)?;
        let row=sqlx::query("SELECT u.id,u.tenant_id,u.username,u.deployment_admin,s.expires_at FROM management_sessions s JOIN management_users u ON u.id=s.user_id WHERE s.token_hash=? AND s.expires_at>?")
            .bind(digest_url(token.as_bytes())).bind(now).fetch_optional(&self.store.pool).await.map_err(|_|ManagementError::Storage)?.ok_or(ManagementError::Unauthorized)?;
        Ok(SessionIdentity {
            deployment_admin: row
                .try_get("deployment_admin")
                .map_err(|_| ManagementError::Storage)?,
            user_id: row
                .try_get::<String, _>("id")
                .map_err(|_| ManagementError::Storage)?
                .parse()
                .map_err(|_| ManagementError::Storage)?,
            tenant_id: row
                .try_get::<String, _>("tenant_id")
                .map_err(|_| ManagementError::Storage)?
                .parse()
                .map_err(|_| ManagementError::Storage)?,
            username: row
                .try_get("username")
                .map_err(|_| ManagementError::Storage)?,
            expires_at: row
                .try_get("expires_at")
                .map_err(|_| ManagementError::Storage)?,
        })
    }
    pub async fn logout(&self, token: &str) -> Result<(), ManagementError> {
        validate_token(token)?;
        sqlx::query("DELETE FROM management_sessions WHERE token_hash=?")
            .bind(digest_url(token.as_bytes()))
            .execute(&self.store.pool)
            .await
            .map_err(|_| ManagementError::Storage)?;
        Ok(())
    }
}
fn validate(username: &str, password: &SecretString) -> Result<(), ManagementError> {
    validate_username(username)
        .and_then(|_| validate_password(password))
        .map_err(|_| ManagementError::Invalid)
}
fn validate_username(username: &str) -> Result<(), ManagementError> {
    if username.is_empty()
        || username.len() > 64
        || !username
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
    {
        return Err(ManagementError::InvalidUsername);
    }
    Ok(())
}
fn validate_password(password: &SecretString) -> Result<(), ManagementError> {
    if password.expose().chars().count() < 12 || password.expose().len() > 1024 {
        return Err(ManagementError::InvalidPassword);
    }
    Ok(())
}
fn validate_token(token: &str) -> Result<(), ManagementError> {
    if token.len() != 43
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(ManagementError::Unauthorized);
    }
    Ok(())
}
async fn hash_password(password: SecretString) -> Result<String, ManagementError> {
    hash_password_with_permit(password, None).await
}
async fn hash_password_with_permit(
    password: SecretString,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Result<String, ManagementError> {
    let mut salt = [0u8; 16];
    crate::fill_random(&mut salt).map_err(|_| ManagementError::Storage)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let salt = SaltString::encode_b64(&salt).map_err(|_| ManagementError::Storage)?;
        Argon2::default()
            .hash_password(password.expose().as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| ManagementError::Storage)
    })
    .await
    .map_err(|_| ManagementError::Storage)?
}
pub fn csrf_token(session: &str) -> Result<String, ManagementError> {
    validate_token(session)?;
    Ok(digest_url(
        format!("codex-rc-bridge/csrf/v1\0{session}").as_bytes(),
    ))
}
