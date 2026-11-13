//! PKCE transactions and account credentials. Only responses from the fixed TLS token
//! endpoint are interpreted as tokens; decoded JWT claims are not management authentication.
use crate::{
    model::AccountIdentity,
    vault::{SecretScope, Vault},
};
use aws_lc_rs::{digest, rand};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Weak},
};
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

pub const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const ENROLL_SCOPE: &str = "codex.remote_control.enroll";
const TRANSACTION_TTL: i64 = 600;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("account sign-in is required")]
    SignInRequired,
    #[error("authorization transaction is missing, expired or already consumed")]
    Transaction,
    #[error("authorization callback is invalid")]
    Callback,
    #[error("authorization was declined")]
    Declined,
    #[error("account, workspace or token validity does not match")]
    Identity,
    #[error("fresh remote-control authorization is required")]
    StepUp,
    #[error("authorization storage failed")]
    Storage,
    #[error("authorization service could not be reached")]
    Network,
    #[error("authorization service rejected the request")]
    Rejected,
    #[error("OAuth token request was rejected")]
    OAuthRequest,
    #[error("authorization response is invalid")]
    Response,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);
impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Deserialize, Serialize)]
pub struct TokenResponse {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
}
pub trait TokenEndpoint: Send + Sync {
    fn token(
        &self,
        form: &[(&str, &str)],
    ) -> impl Future<Output = Result<TokenResponse, AuthError>> + Send;
}

#[derive(Deserialize)]
pub(crate) struct IdentityClaims {
    pub chatgpt_account_id: Option<String>,
    pub account_id: Option<String>,
    pub chatgpt_account_user_id: Option<String>,
    pub account_user_id: Option<String>,
    pub user_id: Option<String>,
}
impl IdentityClaims {
    pub fn workspace(&self) -> Option<&str> {
        self.chatgpt_account_id
            .as_deref()
            .or(self.account_id.as_deref())
    }
    pub fn member(&self) -> Option<&str> {
        self.chatgpt_account_user_id
            .as_deref()
            .or(self.account_user_id.as_deref())
    }
    fn account(&self) -> Result<AccountIdentity, AuthError> {
        let workspace = self.workspace().ok_or(AuthError::Identity)?;
        let user = self
            .member()
            .or(self.user_id.as_deref())
            .ok_or(AuthError::Identity)?;
        for field in [workspace, user] {
            crate::identity::validate_field(field).map_err(|_| AuthError::Identity)?;
        }
        Ok(AccountIdentity {
            workspace_id: workspace.to_owned(),
            user_id: user.to_owned(),
        })
    }
}
#[derive(Deserialize)]
pub(crate) struct Claims {
    #[serde(rename = "https://api.openai.com/auth")]
    pub identity: IdentityClaims,
    pub sub: Option<String>,
    pub exp: f64,
    pub iat: Option<f64>,
    pub pwd_auth_time: Option<f64>,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub scp: Vec<String>,
}
pub(crate) fn claims(token: &SecretString) -> Result<Claims, AuthError> {
    if token.expose().len() > 65536 {
        return Err(AuthError::Response);
    }
    let parts: Vec<_> = token.expose().split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Err(AuthError::Response);
    }
    let raw = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| AuthError::Response)?,
    );
    serde_json::from_slice(&raw).map_err(|_| AuthError::Response)
}

#[derive(Serialize, Deserialize)]
pub struct AccountTokens {
    pub account: AccountIdentity,
    pub(crate) access: SecretString,
    refresh: Option<SecretString>,
    pub(crate) global_user: Option<String>,
    auth_user: Option<String>,
    pub expires_at: f64,
}

/// Display-only metadata from stored upstream credentials, never an authorization identity.
#[derive(Default, Serialize)]
pub struct AccountProfile {
    pub email: Option<String>,
    pub name: Option<String>,
}

impl AccountTokens {
    fn profile(&self) -> AccountProfile {
        let Some(payload) = self.access.expose().split('.').nth(1) else {
            return AccountProfile::default();
        };
        if payload.len() > 65536 {
            return AccountProfile::default();
        }
        let Ok(raw) = URL_SAFE_NO_PAD.decode(payload).map(Zeroizing::new) else {
            return AccountProfile::default();
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            return AccountProfile::default();
        };
        let profile = &value["https://api.openai.com/profile"];
        let text = |field: &str, max: usize| {
            profile[field]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
                .map(str::to_owned)
        };
        AccountProfile {
            email: text("email", 254),
            name: text("name", 200),
        }
    }
}
impl AccountTokens {
    pub fn validate(
        response: TokenResponse,
        expected: Option<&AccountIdentity>,
        now: i64,
    ) -> Result<Self, AuthError> {
        let c = claims(&response.access_token)?;
        let account = c.identity.account()?;
        if expected.is_some_and(|e| e != &account)
            || !c.exp.is_finite()
            || c.exp <= now as f64 + 60.0
        {
            return Err(AuthError::Identity);
        }
        Ok(Self {
            account,
            access: response.access_token,
            refresh: response.refresh_token,
            auth_user: c.identity.user_id.clone(),
            global_user: c.identity.user_id.or(c.sub),
            expires_at: c.exp,
        })
    }
    pub fn access_token(&self) -> &SecretString {
        &self.access
    }
    pub(crate) fn accepts_owner(&self, owner: &str) -> bool {
        !owner.is_empty()
            && (owner == self.account.user_id || self.auth_user.as_deref() == Some(owner))
    }
}

pub fn validate_step_up(
    token: &SecretString,
    account: &AccountTokens,
    owner: &str,
    now: time::OffsetDateTime,
) -> Result<(), AuthError> {
    if !account.accepts_owner(owner) {
        return Err(AuthError::StepUp);
    }
    validate_step_up_for_account(token, account, now)
}

/// Check everything known locally before allocating an upstream registration.
pub(crate) fn validate_step_up_for_account(
    token: &SecretString,
    account: &AccountTokens,
    now: time::OffsetDateTime,
) -> Result<(), AuthError> {
    let c = claims(token)?;
    let issued = c.iat.ok_or(AuthError::StepUp)?;
    let password_time = c.pwd_auth_time.ok_or(AuthError::StepUp)?;
    let global = c.identity.user_id.as_deref().or(c.sub.as_deref());
    let scope: std::collections::BTreeSet<_> = c
        .scope
        .split_whitespace()
        .chain(c.scp.iter().map(String::as_str).filter(|s| !s.is_empty()))
        .collect();
    let member_ok = c.identity.member() == Some(account.account.user_id.as_str());
    if c.identity.workspace() != Some(account.account.workspace_id.as_str())
        || !member_ok
        || (global.is_some()
            && account.global_user.is_some()
            && global != account.global_user.as_deref())
        || scope.len() != 1
        || !scope.contains(ENROLL_SCOPE)
        || !issued.is_finite()
        || now.unix_timestamp() as f64 - issued > 300.0
        || !password_time.is_finite()
        || (now.unix_timestamp_nanos() / 1_000_000) as f64 - password_time > 300_000.0
        || !c.exp.is_finite()
        || c.exp <= now.unix_timestamp() as f64
    {
        return Err(AuthError::StepUp);
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
pub enum LoginPurpose {
    Account {
        expected: Option<AccountIdentity>,
    },
    OutStepUp {
        device_id: Uuid,
        account: AccountIdentity,
    },
}
#[derive(Serialize, Deserialize)]
struct Pending {
    state: SecretString,
    verifier: SecretString,
    redirect: String,
    purpose: LoginPurpose,
    expires_at: i64,
}
// Authorization URL contains one-time state; expose it only to the initiating user, never logs.
#[derive(Serialize)]
pub struct LoginStart {
    pub id: Uuid,
    pub url: String,
    pub expires_at: i64,
}
pub enum LoginResult {
    Account(AccountTokens),
    StepUp {
        device_id: Uuid,
        token: SecretString,
        account: AccountIdentity,
    },
}

type AccountLockKey = (Uuid, String, String);
type AccountLocks = Mutex<HashMap<AccountLockKey, Weak<Mutex<()>>>>;
pub struct AuthService<T> {
    vault: Vault,
    transport: T,
    locks: AccountLocks,
}
impl<T: TokenEndpoint> AuthService<T> {
    pub(crate) async fn ensure_endpoint_usable(
        &self,
        endpoint: &crate::model::Endpoint,
    ) -> Result<(), AuthError> {
        let current = self
            .vault
            .store
            .endpoint(endpoint.tenant_id, endpoint.device_id)
            .await
            .map_err(|_| AuthError::Storage)?;
        if !current.is_some_and(|current| {
            current.account == endpoint.account
                && current.kind == endpoint.kind
                && current.identity_name == endpoint.identity_name
        }) || self
            .vault
            .store
            .endpoint_deleting(endpoint.tenant_id, endpoint.device_id)
            .await
            .map_err(|_| AuthError::Storage)?
        {
            return Err(AuthError::Identity);
        }
        Ok(())
    }
    pub fn new(vault: Vault, transport: T) -> Self {
        Self {
            vault,
            transport,
            locks: Mutex::new(HashMap::new()),
        }
    }
    async fn account_lock(&self, tenant: Uuid, account: &AccountIdentity) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() != 0);
        let key = (
            tenant,
            account.workspace_id.clone(),
            account.user_id.clone(),
        );
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }
    pub async fn begin(
        &self,
        tenant: Uuid,
        purpose: LoginPurpose,
        port: u16,
        now: i64,
    ) -> Result<LoginStart, AuthError> {
        if ![1455, 1457].contains(&port) {
            return Err(AuthError::Callback);
        }
        if let LoginPurpose::OutStepUp { device_id, account } = &purpose {
            if self
                .vault
                .store
                .endpoint_deleting(tenant, *device_id)
                .await
                .map_err(|_| AuthError::Storage)?
            {
                return Err(AuthError::Identity);
            }
            let endpoint = self
                .vault
                .store
                .endpoint(tenant, *device_id)
                .await
                .map_err(|_| AuthError::Storage)?;
            if !endpoint
                .is_some_and(|e| e.account == *account && e.kind == crate::model::EndpointKind::Out)
            {
                return Err(AuthError::Identity);
            }
        }
        let random = || -> Result<SecretString, AuthError> {
            let mut bytes = Zeroizing::new([0; 32]);
            rand::fill(bytes.as_mut()).map_err(|_| AuthError::Response)?;
            Ok(SecretString::new(URL_SAFE_NO_PAD.encode(bytes.as_ref())))
        };
        let state = random()?;
        let verifier = random()?;
        let redirect = format!("http://localhost:{port}/auth/callback");
        let mut url = Url::parse("https://auth.openai.com/oauth/authorize")
            .map_err(|_| AuthError::Response)?;
        let challenge = URL_SAFE_NO_PAD.encode(digest::digest(
            &digest::SHA256,
            verifier.expose().as_bytes(),
        ));
        {
            let mut q = url.query_pairs_mut();
            q.extend_pairs([
                ("response_type", "code"),
                ("client_id", OAUTH_CLIENT_ID),
                ("redirect_uri", &redirect),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("state", state.expose()),
                ("originator", "codex_rc_bridge"),
                ("codex_cli_simplified_flow", "true"),
            ]);
            match &purpose {
                LoginPurpose::Account { .. } => {
                    q.append_pair("scope", "openid profile email offline_access");
                }
                LoginPurpose::OutStepUp { account, .. } => {
                    q.extend_pairs([
                        ("scope", ENROLL_SCOPE),
                        ("reauth", "remote_control"),
                        ("max_age", "0"),
                        ("allowed_workspace_id", account.workspace_id.as_str()),
                        ("current_workspace_id", account.workspace_id.as_str()),
                    ]);
                }
            }
        }
        let expires_at = now
            .checked_add(TRANSACTION_TTL)
            .ok_or(AuthError::Transaction)?;
        let pending = Pending {
            state,
            verifier,
            redirect,
            purpose,
            expires_at,
        };
        let id = Uuid::new_v4();
        let bytes = Zeroizing::new(serde_json::to_vec(&pending).map_err(|_| AuthError::Storage)?);
        let encrypted = self
            .vault
            .key
            .seal(tenant, &SecretScope::OAuth(id), &bytes)
            .map_err(|_| AuthError::Storage)?;
        let mut tx = self
            .vault
            .store
            .pool
            .begin()
            .await
            .map_err(|_| AuthError::Storage)?;
        sqlx::query("DELETE FROM oauth_transactions WHERE tenant_id=? AND expires_at<=?")
            .bind(tenant.to_string())
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|_| AuthError::Storage)?;
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM oauth_transactions WHERE tenant_id=?")
                .bind(tenant.to_string())
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| AuthError::Storage)?;
        if count >= 32 {
            return Err(AuthError::Transaction);
        }
        sqlx::query(
            "INSERT INTO oauth_transactions(tenant_id,id,expires_at,ciphertext) VALUES(?,?,?,?)",
        )
        .bind(tenant.to_string())
        .bind(id.to_string())
        .bind(expires_at)
        .bind(encrypted)
        .execute(&mut *tx)
        .await
        .map_err(|_| AuthError::Storage)?;
        tx.commit().await.map_err(|_| AuthError::Storage)?;
        Ok(LoginStart {
            id,
            url: url.into(),
            expires_at,
        })
    }
    /// Consume before any network request. Unknown/mismatched callbacks cannot burn another transaction.
    pub async fn complete(
        &self,
        tenant: Uuid,
        id: Uuid,
        callback: &str,
        now: i64,
    ) -> Result<LoginResult, AuthError> {
        let ciphertext: Vec<u8> = sqlx::query_scalar(
            "SELECT ciphertext FROM oauth_transactions WHERE tenant_id=? AND id=? AND expires_at>?",
        )
        .bind(tenant.to_string())
        .bind(id.to_string())
        .bind(now)
        .fetch_optional(&self.vault.store.pool)
        .await
        .map_err(|_| AuthError::Storage)?
        .ok_or(AuthError::Transaction)?;
        let bytes = self
            .vault
            .key
            .open(tenant, &SecretScope::OAuth(id), &ciphertext)
            .map_err(|_| AuthError::Storage)?;
        let pending: Pending = serde_json::from_slice(&bytes).map_err(|_| AuthError::Storage)?;
        if let LoginPurpose::OutStepUp { device_id, .. } = &pending.purpose
            && (self
                .vault
                .store
                .endpoint_deleting(tenant, *device_id)
                .await
                .map_err(|_| AuthError::Storage)?
                || self
                    .vault
                    .store
                    .endpoint(tenant, *device_id)
                    .await
                    .map_err(|_| AuthError::Storage)?
                    .is_none())
        {
            return Err(AuthError::Transaction);
        }
        if pending.expires_at <= now {
            return Err(AuthError::Transaction);
        }
        let code = parse_callback(callback, &pending)?;
        let consumed = sqlx::query(
            "DELETE FROM oauth_transactions WHERE tenant_id=? AND id=? AND ciphertext=?",
        )
        .bind(tenant.to_string())
        .bind(id.to_string())
        .bind(ciphertext)
        .execute(&self.vault.store.pool)
        .await
        .map_err(|_| AuthError::Storage)?
        .rows_affected();
        if consumed != 1 {
            return Err(AuthError::Transaction);
        }
        let code = code.ok_or(AuthError::Declined)?;
        let response = self
            .transport
            .token(&[
                ("grant_type", "authorization_code"),
                ("code", code.expose()),
                ("code_verifier", pending.verifier.expose()),
                ("client_id", OAUTH_CLIENT_ID),
                ("redirect_uri", &pending.redirect),
            ])
            .await?;
        match pending.purpose {
            LoginPurpose::Account { expected } => {
                let tokens = AccountTokens::validate(response, expected.as_ref(), now)?;
                let lock = self.account_lock(tenant, &tokens.account).await;
                let _guard = lock.lock().await;
                self.save_account(tenant, &tokens).await?;
                self.vault
                    .store
                    .set_sign_in_required(tenant, &tokens.account, false)
                    .await
                    .map_err(|_| AuthError::Storage)?;
                Ok(LoginResult::Account(tokens))
            }
            LoginPurpose::OutStepUp { device_id, account } => {
                // Enrollment additionally validates fresh proof, scope and chosen registration owner.
                let c = claims(&response.access_token)?;
                if c.identity.workspace() != Some(account.workspace_id.as_str()) {
                    return Err(AuthError::Identity);
                }
                Ok(LoginResult::StepUp {
                    device_id,
                    token: response.access_token,
                    account,
                })
            }
        }
    }
    pub(crate) async fn invalidate_endpoint(
        &self,
        tenant: Uuid,
        device: Uuid,
    ) -> Result<(), AuthError> {
        use sqlx::Row;
        let rows = sqlx::query("SELECT id,ciphertext FROM oauth_transactions WHERE tenant_id=?")
            .bind(tenant.to_string())
            .fetch_all(&self.vault.store.pool)
            .await
            .map_err(|_| AuthError::Storage)?;
        for row in rows {
            let id: String = row.try_get("id").map_err(|_| AuthError::Storage)?;
            let id: Uuid = id.parse().map_err(|_| AuthError::Storage)?;
            let ciphertext: Vec<u8> = row.try_get("ciphertext").map_err(|_| AuthError::Storage)?;
            let raw = self
                .vault
                .key
                .open(tenant, &SecretScope::OAuth(id), &ciphertext)
                .map_err(|_| AuthError::Storage)?;
            let pending: Pending = serde_json::from_slice(&raw).map_err(|_| AuthError::Storage)?;
            if matches!(pending.purpose,LoginPurpose::OutStepUp{device_id,..} if device_id==device)
            {
                sqlx::query(
                    "DELETE FROM oauth_transactions WHERE tenant_id=? AND id=? AND ciphertext=?",
                )
                .bind(tenant.to_string())
                .bind(id.to_string())
                .bind(ciphertext)
                .execute(&self.vault.store.pool)
                .await
                .map_err(|_| AuthError::Storage)?;
            }
        }
        Ok(())
    }
    async fn save_account(&self, tenant: Uuid, tokens: &AccountTokens) -> Result<(), AuthError> {
        let scope = SecretScope::Account(tokens.account.clone());
        let raw = Zeroizing::new(serde_json::to_vec(tokens).map_err(|_| AuthError::Storage)?);
        let encrypted = self
            .vault
            .key
            .seal(tenant, &scope, &raw)
            .map_err(|_| AuthError::Storage)?;
        let scope = crate::vault::scope_key(&scope).map_err(|_| AuthError::Storage)?;
        let mut tx = self
            .vault
            .store
            .pool
            .begin()
            .await
            .map_err(|_| AuthError::Storage)?;
        sqlx::query("INSERT INTO accounts(tenant_id,workspace_id,user_id) VALUES(?,?,?) ON CONFLICT DO NOTHING")
            .bind(tenant.to_string()).bind(&tokens.account.workspace_id).bind(&tokens.account.user_id).execute(&mut *tx).await.map_err(|_| AuthError::Storage)?;
        sqlx::query("INSERT INTO secrets(tenant_id,scope,ciphertext) VALUES(?,?,?) ON CONFLICT(tenant_id,scope) DO UPDATE SET ciphertext=excluded.ciphertext")
            .bind(tenant.to_string()).bind(scope).bind(encrypted).execute(&mut *tx).await.map_err(|_| AuthError::Storage)?;
        tx.commit().await.map_err(|_| AuthError::Storage)
    }
    /// Reading a label must not refresh credentials or require an online upstream.
    pub async fn cached_profile(
        &self,
        tenant: Uuid,
        account: &AccountIdentity,
    ) -> Result<AccountProfile, AuthError> {
        let Some(raw) = self
            .vault
            .read(tenant, &SecretScope::Account(account.clone()))
            .await
            .map_err(|_| AuthError::Storage)?
        else {
            return Ok(AccountProfile::default());
        };
        let tokens: AccountTokens = serde_json::from_slice(&raw).map_err(|_| AuthError::Storage)?;
        if &tokens.account != account {
            return Ok(AccountProfile::default());
        }
        Ok(tokens.profile())
    }

    /// The lock covers load, exchange, validation AND durable replacement, coalescing rotated refresh tokens.
    pub async fn account(
        &self,
        tenant: Uuid,
        account: &AccountIdentity,
        rejected: Option<&SecretString>,
        now: i64,
    ) -> Result<AccountTokens, AuthError> {
        let lock = self.account_lock(tenant, account).await;
        let _guard = lock.lock().await;
        self.account_attempt(tenant, account, rejected, now, false)
            .await
    }
    async fn account_attempt(
        &self,
        tenant: Uuid,
        account: &AccountIdentity,
        rejected: Option<&SecretString>,
        now: i64,
        force: bool,
    ) -> Result<AccountTokens, AuthError> {
        let result = self
            .account_locked(tenant, account, rejected, now, force)
            .await;
        match result {
            Err(AuthError::Rejected | AuthError::SignInRequired) => {
                self.vault
                    .store
                    .set_sign_in_required(tenant, account, true)
                    .await
                    .map_err(|_| AuthError::Storage)?;
                Err(AuthError::SignInRequired)
            }
            result => result,
        }
    }
    /// Capture the failure revision before checking. Marked accounts must prove refreshability.
    pub(crate) async fn begin_check(
        &self,
        tenant: Uuid,
        account: &AccountIdentity,
    ) -> Result<i64, AuthError> {
        let lock = self.account_lock(tenant, account).await;
        let _guard = lock.lock().await;
        let (required, revision): (bool, i64) = sqlx::query_as("SELECT sign_in_required,sign_in_revision FROM accounts WHERE tenant_id=? AND workspace_id=? AND user_id=?")
            .bind(tenant.to_string()).bind(&account.workspace_id).bind(&account.user_id)
            .fetch_one(&self.vault.store.pool).await.map_err(|_| AuthError::Storage)?;
        self.account_attempt(
            tenant,
            account,
            None,
            time::OffsetDateTime::now_utc().unix_timestamp(),
            required,
        )
        .await?;
        Ok(revision)
    }
    pub(crate) async fn confirm_check(
        &self,
        tenant: Uuid,
        account: &AccountIdentity,
        revision: i64,
    ) -> Result<(), AuthError> {
        let updated = sqlx::query("UPDATE accounts SET sign_in_required=0 WHERE tenant_id=? AND workspace_id=? AND user_id=? AND sign_in_revision=?")
            .bind(tenant.to_string()).bind(&account.workspace_id).bind(&account.user_id).bind(revision)
            .execute(&self.vault.store.pool).await.map_err(|_| AuthError::Storage)?.rows_affected();
        if updated == 0
            && self
                .vault
                .store
                .sign_in_required(tenant, account)
                .await
                .map_err(|_| AuthError::Storage)?
        {
            return Err(AuthError::SignInRequired);
        }
        Ok(())
    }
    async fn account_locked(
        &self,
        tenant: Uuid,
        account: &AccountIdentity,
        rejected: Option<&SecretString>,
        now: i64,
        force: bool,
    ) -> Result<AccountTokens, AuthError> {
        let raw = self
            .vault
            .read(tenant, &SecretScope::Account(account.clone()))
            .await
            .map_err(|_| AuthError::Storage)?
            .ok_or(AuthError::SignInRequired)?;
        let mut current: AccountTokens =
            serde_json::from_slice(&raw).map_err(|_| AuthError::Storage)?;
        if &current.account != account {
            return Err(AuthError::SignInRequired);
        }
        let refresh = current.refresh.as_ref().ok_or(AuthError::SignInRequired)?;
        let force = force || rejected.is_some_and(|r| r.expose() == current.access.expose());
        if force || current.expires_at <= now as f64 + 120.0 {
            let response = self
                .transport
                .token(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh.expose()),
                    ("client_id", OAUTH_CLIENT_ID),
                ])
                .await?;
            let mut updated = AccountTokens::validate(response, Some(account), now).map_err(
                |error| match error {
                    AuthError::Identity => AuthError::SignInRequired,
                    error => error,
                },
            )?;
            if current.global_user.is_some() && updated.global_user != current.global_user {
                return Err(AuthError::SignInRequired);
            }
            if updated.refresh.is_none() {
                updated.refresh = current.refresh.take();
            }
            self.save_account(tenant, &updated).await?;
            current = updated;
        }
        Ok(current)
    }
}

fn parse_callback(callback: &str, pending: &Pending) -> Result<Option<SecretString>, AuthError> {
    if callback.len() > 16384 {
        return Err(AuthError::Callback);
    }
    let url = Url::parse(callback).map_err(|_| AuthError::Callback)?;
    let redirect = Url::parse(&pending.redirect).map_err(|_| AuthError::Callback)?;
    if url.scheme() != redirect.scheme()
        || url.host_str() != redirect.host_str()
        || url.port() != redirect.port()
        || url.path() != redirect.path()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(AuthError::Callback);
    }
    let mut values = HashMap::new();
    for (key, value) in url.query_pairs() {
        if values
            .insert(key.into_owned(), SecretString::new(value.into_owned()))
            .is_some()
        {
            return Err(AuthError::Callback);
        }
    }
    if values.get("state").map(SecretString::expose) != Some(pending.state.expose()) {
        return Err(AuthError::Callback);
    }
    match (values.remove("code"), values.remove("error")) {
        (Some(code), None) if !code.expose().is_empty() => Ok(Some(code)),
        (None, Some(error)) if !error.expose().is_empty() => Ok(None),
        _ => Err(AuthError::Callback),
    }
}
