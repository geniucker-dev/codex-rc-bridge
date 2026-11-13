//! Fixed-origin HTTP control plane. Redirects and implicit process-wide proxy settings are disabled.
mod clients;
#[cfg(test)]
mod tests;
use crate::{
    auth::{AuthError, AuthService, SecretString, TokenEndpoint, TokenResponse},
    device::{self, ControllerSession, EnrollmentChallenge, EnrollmentMode, Registration},
    identity::OutSigningKey,
    model::AccountIdentity,
};
pub use clients::PairedClient;
use reqwest::{Client, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;
use zeroize::Zeroizing;

const MAX_RESPONSE: usize = 2 * 1024 * 1024;

type HostLocks = std::sync::Arc<
    tokio::sync::Mutex<
        std::collections::HashMap<(Uuid, Uuid), std::sync::Weak<tokio::sync::Mutex<()>>>,
    >,
>;

#[derive(Clone)]
pub struct Upstream {
    network: std::sync::Arc<std::sync::RwLock<Network>>,
    api_base: String,
    token_url: String,
    host_locks: HostLocks,
}
#[derive(Clone)]
struct Network {
    client: Client,
    proxy: Option<String>,
}
impl Upstream {
    fn client(&self) -> Client {
        self.network.read().expect("network lock").client.clone()
    }
    pub fn proxy_url(&self) -> Option<String> {
        self.network.read().expect("network lock").proxy.clone()
    }
    /// Caller drains every runtime before replacement; clones share the new HTTP/WS policy.
    pub fn replace_network(&self, prepared: &Self) {
        let next = prepared.network.read().expect("network lock").clone();
        *self.network.write().expect("network lock") = next;
    }
    /// None means direct, not "inherit environment". Explicit proxy policy belongs to this client.
    pub fn new(proxy: Option<&str>) -> Result<Self, AuthError> {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(15))
            .user_agent(user_agent());
        if let Some(proxy) = proxy {
            let url = url::Url::parse(proxy).map_err(|_| AuthError::Network)?;
            if url.scheme() != "http"
                || url.host_str().is_none()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(AuthError::Network);
            }
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|_| AuthError::Network)?);
        }
        Ok(Self {
            network: std::sync::Arc::new(std::sync::RwLock::new(Network {
                client: builder.build().map_err(|_| AuthError::Network)?,
                proxy: proxy.map(str::to_owned),
            })),
            api_base: "https://chatgpt.com/backend-api".into(),
            token_url: "https://auth.openai.com/oauth/token".into(),
            host_locks: Default::default(),
        })
    }
    pub(crate) fn websocket_url(&self, side: crate::wire::Side) -> String {
        let path = match side {
            crate::wire::Side::In => "/wham/remote/control/server",
            crate::wire::Side::Out => "/codex/remote/control/client",
        };
        format!(
            "{}{path}",
            self.api_base
                .replacen("https://", "wss://", 1)
                .replacen("http://", "ws://", 1)
        )
    }
    #[cfg(test)]
    pub(crate) fn local_test(origin: &str) -> Self {
        Self {
            network: std::sync::Arc::new(std::sync::RwLock::new(Network {
                client: Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(3))
                    .build()
                    .unwrap(),
                proxy: None,
            })),
            api_base: format!("{origin}/backend-api"),
            token_url: format!("{origin}/oauth/token"),
            host_locks: Default::default(),
        }
    }
}
pub fn user_agent() -> String {
    crate::user_agent::get()
}
fn device_arch() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "arm64",
        ("windows", "x86_64") => "AMD64",
        ("windows", "aarch64") => "ARM64",
        (_, arch) => arch,
    }
}
pub fn unix_time() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Device(#[from] device::DeviceError),
    #[error("controller registration must be authorized again")]
    Registration,
    #[error("host registration no longer exists")]
    HostRegistrationMissing,
    #[error("host registration could not be completed; retry with the same device identity")]
    HostRegistrationRetry,
    #[error("control service rejected the request (HTTP {0})")]
    Rejected(u16),
    #[error("control response has invalid metadata")]
    Invalid,
}
pub(crate) fn controller_authorization_error(error: ControlError) -> ControlError {
    match error {
        ControlError::Rejected(401 | 403 | 404) => ControlError::Registration,
        error => error,
    }
}
#[derive(Deserialize)]
struct Start {
    client_id: String,
    account_user_id: String,
    device_key_challenge: EnrollmentChallenge,
}
// Persist registration before checking the challenge or preparing transient proof material.
pub(crate) struct ControllerEnrollment {
    pub registration: Registration,
    challenge: EnrollmentChallenge,
}
pub(crate) struct ControllerRefresh {
    pub registration: Registration,
    body: Zeroizing<Vec<u8>>,
}
struct ControlRequest<'a> {
    method: reqwest::Method,
    path: &'a str,
    body: Option<&'a [u8]>,
    installation: Option<Uuid>,
    refresh_start: bool,
}

impl Upstream {
    /// Only known account-authenticated operations call this helper. Bodies are frozen before the 401 retry.
    async fn control<T: TokenEndpoint, R: DeserializeOwned>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        path: &str,
        body: &[u8],
        refresh_start: bool,
    ) -> Result<R, ControlError> {
        self.account_request(
            auth,
            tenant,
            account,
            ControlRequest {
                method: reqwest::Method::POST,
                path,
                body: Some(body),
                installation: None,
                refresh_start,
            },
        )
        .await
    }
    async fn account_request<T: TokenEndpoint, R: DeserializeOwned>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        request: ControlRequest<'_>,
    ) -> Result<R, ControlError> {
        let mut credentials = auth.account(tenant, account, None, unix_time()).await?;
        for attempt in 0..2 {
            let mut builder = self
                .client()
                .request(
                    request.method.clone(),
                    format!("{}{}", self.api_base, request.path),
                )
                .header("originator", "codex_rc_bridge")
                .header("ChatGPT-Account-Id", &account.workspace_id)
                .bearer_auth(credentials.access_token().expose())
                .header("Content-Type", "application/json");
            if let Some(body) = request.body {
                builder = builder.body(body.to_vec());
            }
            if let Some(installation) = request.installation {
                builder = builder.header("x-codex-installation-id", installation.to_string());
            }
            let response = builder.send().await.map_err(|_| AuthError::Network)?;
            let status = response.status();
            if status.is_success() {
                return Ok(decode(response).await?);
            }
            // Desktop treats only these exact refresh failures as lost registration.
            if request.refresh_start && matches!(status.as_u16(), 401 | 403 | 404) {
                let error: serde_json::Value = decode(response).await.unwrap_or_default();
                let detail = error.get("error").unwrap_or(&error);
                let message = detail
                    .get("message")
                    .or_else(|| detail.get("detail"))
                    .and_then(|v| v.as_str());
                if matches!(
                    (status.as_u16(), message),
                    (404, Some("Remote-control client not found"))
                        | (403, Some("Remote-control client has been revoked"))
                ) {
                    return Err(ControlError::Registration);
                }
            }
            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                credentials = auth
                    .account(
                        tenant,
                        account,
                        Some(credentials.access_token()),
                        unix_time(),
                    )
                    .await?;
                continue;
            }
            return Err(ControlError::Rejected(status.as_u16()));
        }
        Err(ControlError::Invalid)
    }
    pub(crate) async fn start_controller_enrollment<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        key: &OutSigningKey,
    ) -> Result<ControllerEnrollment, ControlError> {
        let credentials = auth.account(tenant, account, None, unix_time()).await?;
        let start: Start = self
            .control(
                auth,
                tenant,
                account,
                "/codex/remote/control/client/enroll/start",
                b"{}",
                false,
            )
            .await?;
        if start.client_id.is_empty() || !credentials.accepts_owner(&start.account_user_id) {
            return Err(ControlError::Invalid);
        }
        let registration = Registration {
            client_id: start.client_id,
            account_user_id: start.account_user_id,
            key_id: device::identity(key).key_id,
        };
        Ok(ControllerEnrollment {
            registration,
            challenge: start.device_key_challenge,
        })
    }
    pub(crate) async fn finish_controller_enrollment<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        key: &OutSigningKey,
        step_up: &SecretString,
        enrollment: ControllerEnrollment,
    ) -> Result<Registration, ControlError> {
        let credentials = auth.account(tenant, account, None, unix_time()).await?;
        let registration = &enrollment.registration;
        device::authorize_enrollment(
            step_up,
            &credentials,
            registration,
            time::OffsetDateTime::now_utc(),
        )?;
        let proof = device::enrollment_proof(
            key,
            registration,
            &enrollment.challenge,
            EnrollmentMode::Enroll,
            unix_time() as u64,
        )?;
        #[derive(Serialize)]
        struct Finish<'a> {
            client_id: &'a str,
            step_up_token: &'a SecretString,
            device_identity: device::DeviceIdentity,
            device_key_proof: device::EnrollmentProof,
        }
        let body = Zeroizing::new(
            serde_json::to_vec(&Finish {
                client_id: &registration.client_id,
                step_up_token: step_up,
                device_identity: device::identity(key),
                device_key_proof: proof,
            })
            .map_err(|_| ControlError::Invalid)?,
        );
        let session: ControllerSession = self
            .control(
                auth,
                tenant,
                account,
                "/codex/remote/control/client/enroll/finish",
                &body,
                false,
            )
            .await?;
        session.validate(&enrollment.registration, unix_time())?;
        Ok(enrollment.registration)
    }
    pub async fn refresh_controller<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        key: &OutSigningKey,
        registration: &Registration,
    ) -> Result<ControllerSession, ControlError> {
        let refresh = self
            .prepare_controller_refresh(auth, tenant, account, key, &registration.client_id)
            .await
            .map_err(controller_authorization_error)?;
        if refresh.registration.account_user_id != registration.account_user_id
            || refresh.registration.key_id != registration.key_id
        {
            return Err(ControlError::Invalid);
        }
        self.finish_controller_refresh(auth, tenant, account, refresh)
            .await
            .map_err(controller_authorization_error)
    }
    /// The upstream challenge must bind this exact client to the original local public key.
    /// Used to restore a missing checkpoint, never to enumerate or guess client identities.
    pub(crate) async fn prepare_controller_refresh<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        key: &OutSigningKey,
        client_id: &str,
    ) -> Result<ControllerRefresh, ControlError> {
        crate::identity::validate_field(client_id).map_err(|_| ControlError::Invalid)?;
        let body = serde_json::to_vec(&serde_json::json!({"client_id":client_id}))
            .map_err(|_| ControlError::Invalid)?;
        let start: Start = self
            .control(
                auth,
                tenant,
                account,
                "/codex/remote/control/client/refresh/start",
                &body,
                true,
            )
            .await?;
        let credentials = auth.account(tenant, account, None, unix_time()).await?;
        if start.client_id != client_id || !credentials.accepts_owner(&start.account_user_id) {
            return Err(ControlError::Invalid);
        }
        let registration = Registration {
            client_id: start.client_id,
            account_user_id: start.account_user_id,
            key_id: device::identity(key).key_id,
        };
        let proof = device::enrollment_proof(
            key,
            &registration,
            &start.device_key_challenge,
            EnrollmentMode::Refresh,
            unix_time() as u64,
        )?;
        #[derive(Serialize)]
        struct Finish<'a> {
            client_id: &'a str,
            device_key_proof: device::EnrollmentProof,
        }
        let body = Zeroizing::new(
            serde_json::to_vec(&Finish {
                client_id: &registration.client_id,
                device_key_proof: proof,
            })
            .map_err(|_| ControlError::Invalid)?,
        );
        Ok(ControllerRefresh { registration, body })
    }
    pub(crate) async fn finish_controller_refresh<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        refresh: ControllerRefresh,
    ) -> Result<ControllerSession, ControlError> {
        let session: ControllerSession = self
            .control(
                auth,
                tenant,
                account,
                "/codex/remote/control/client/refresh/finish",
                &refresh.body,
                false,
            )
            .await?;
        session.validate(&refresh.registration, unix_time())?;
        Ok(session)
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct HostRegistration {
    pub server_id: String,
    pub environment_id: String,
}
#[derive(Deserialize)]
pub struct HostSession {
    pub server_id: String,
    pub environment_id: String,
    pub expires_at: String,
    pub remote_control_token: SecretString,
}
impl HostSession {
    pub fn registration(&self) -> HostRegistration {
        HostRegistration {
            server_id: self.server_id.clone(),
            environment_id: self.environment_id.clone(),
        }
    }
    pub fn expiry(&self) -> Result<i64, ControlError> {
        time::OffsetDateTime::parse(
            &self.expires_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map(|t| t.unix_timestamp())
        .map_err(|_| ControlError::Invalid)
    }
    fn validate(&self, previous: Option<&HostRegistration>) -> Result<(), ControlError> {
        if self.server_id.is_empty()
            || self.environment_id.is_empty()
            || self.remote_control_token.expose().is_empty()
            || self.expiry()? <= unix_time()
            || previous.is_some_and(|p| {
                p.server_id != self.server_id || p.environment_id != self.environment_id
            })
        {
            return Err(ControlError::Invalid);
        }
        Ok(())
    }
}
#[derive(Deserialize, Serialize)]
pub struct Environment {
    pub env_id: String,
    pub online: Option<bool>,
    pub last_seen_at: Option<String>,
    pub name: Option<String>,
    pub display_name: Option<String>,
    #[serde(default)]
    pub host_name: Option<String>,
    pub app_server_version: Option<String>,
}
impl Environment {
    /// Choose the first non-empty label: display_name, name, then host_name.
    pub fn label(&self) -> Option<String> {
        [&self.display_name, &self.name, &self.host_name]
            .into_iter()
            .flatten()
            .map(|value| value.trim())
            .find(|value| !value.is_empty())
            .filter(|value| value.chars().count() <= 200 && !value.chars().any(char::is_control))
            .map(str::to_owned)
    }
}
#[derive(Deserialize)]
struct EnvironmentPage {
    items: Vec<Environment>,
    cursor: Option<String>,
}
#[derive(Deserialize)]
pub struct PairingCode {
    pub manual_pairing_code: SecretString,
    server_id: String,
    environment_id: String,
    expires_at: String,
}
impl PairingCode {
    pub(crate) fn expires_unix(&self) -> Result<i64, ControlError> {
        time::OffsetDateTime::parse(
            &self.expires_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map(|value| value.unix_timestamp())
        .map_err(|_| ControlError::Invalid)
    }
}
impl ControlError {
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Auth(AuthError::Network)
                | Self::Rejected(408 | 429 | 500..=599)
                | Self::HostRegistrationRetry
        )
    }
}
impl Upstream {
    /// Serialize refresh/replacement for one IN; publish only durably saved registrations.
    pub(crate) async fn registered_host_session<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        vault: &crate::vault::Vault,
        endpoint: &crate::model::Endpoint,
        version: &str,
        stale_server: Option<&str>,
    ) -> Result<HostSession, ControlError> {
        let lock = {
            let mut locks = self.host_locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() != 0);
            let entry = locks
                .entry((endpoint.tenant_id, endpoint.device_id))
                .or_default();
            match entry.upgrade() {
                Some(lock) => lock,
                None => {
                    let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                    *entry = std::sync::Arc::downgrade(&lock);
                    lock
                }
            }
        };
        let _guard = lock.lock().await;
        if endpoint.kind != crate::model::EndpointKind::In {
            return Err(ControlError::Invalid);
        }
        auth.ensure_endpoint_usable(endpoint).await?;
        let previous = vault
            .read(
                endpoint.tenant_id,
                &crate::vault::SecretScope::InRegistration(endpoint.device_id),
            )
            .await
            .map_err(|_| AuthError::Storage)?;
        let previous: Option<HostRegistration> = previous
            .map(|raw| serde_json::from_slice(&raw))
            .transpose()
            .map_err(|_| ControlError::Invalid)?;
        if previous.is_none() {
            if let Some(environment) = vault
                .store
                .host_environment(endpoint.tenant_id, endpoint.device_id)
                .await
                .map_err(|_| AuthError::Storage)?
            {
                if !self
                    .revoke_environment_clients(
                        auth,
                        endpoint.tenant_id,
                        &endpoint.account,
                        &environment,
                    )
                    .await?
                {
                    return Err(ControlError::HostRegistrationRetry);
                }
            } else {
                // Only an unfinished first registration has no cleanup identity.
                let draft: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints WHERE tenant_id=? AND device_id=? AND kind='in' AND lifecycle='draft')")
                    .bind(endpoint.tenant_id.to_string()).bind(endpoint.device_id.to_string())
                    .fetch_one(&vault.store.pool).await.map_err(|_| AuthError::Storage)?;
                if !draft {
                    return Err(ControlError::HostRegistrationMissing);
                }
            }
        }
        if let Some(previous) = previous.as_ref()
            && stale_server != Some(previous.server_id.as_str())
        {
            match self
                .host_session(auth, endpoint, Some(previous), version)
                .await
            {
                Err(ControlError::HostRegistrationMissing) => {}
                result => return result,
            }
        }
        // Codex retries enroll using its persistent installation ID. A lost reply
        // does not permanently block this IN. The lock also coalesces stale notices.
        self.finish_host_enrollment(auth, vault, endpoint, version)
            .await
    }
    /// Create or recover an IN using its persistent identity and serialized registration flow.
    pub async fn enroll_host<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        vault: &crate::vault::Vault,
        endpoint: &crate::model::Endpoint,
        version: &str,
    ) -> Result<HostSession, ControlError> {
        self.registered_host_session(auth, vault, endpoint, version, None)
            .await
    }
    async fn finish_host_enrollment<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        vault: &crate::vault::Vault,
        endpoint: &crate::model::Endpoint,
        version: &str,
    ) -> Result<HostSession, ControlError> {
        let session = self
            .request_host_session(auth, endpoint, None, version)
            .await
            .map_err(|error| match error {
                ControlError::Invalid | ControlError::Auth(AuthError::Response) => {
                    ControlError::HostRegistrationRetry
                }
                error => error,
            })?;
        // Finish an already-sent request before the caller joins us for deletion.
        // The atomic save preserves `deleting` and supplies its cleanup identity.
        vault
            .save_host_registration(
                endpoint.tenant_id,
                endpoint.device_id,
                &session.registration(),
            )
            .await
            .map_err(|_| ControlError::HostRegistrationRetry)?;
        Ok(session)
    }
    pub async fn host_session<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        endpoint: &crate::model::Endpoint,
        previous: Option<&HostRegistration>,
        version: &str,
    ) -> Result<HostSession, ControlError> {
        if endpoint.kind != crate::model::EndpointKind::In {
            return Err(ControlError::Invalid);
        }
        auth.ensure_endpoint_usable(endpoint).await?;
        self.request_host_session(auth, endpoint, previous, version)
            .await
    }
    // Once a request starts, deletion joins its worker and uses the saved
    // registration. Check lifecycle before starting, not halfway through it.
    async fn request_host_session<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        endpoint: &crate::model::Endpoint,
        previous: Option<&HostRegistration>,
        version: &str,
    ) -> Result<HostSession, ControlError> {
        let (path, body) = match previous {
            Some(registration) => (
                "/wham/remote/control/server/refresh",
                serde_json::json!({"server_id":registration.server_id,"installation_id":endpoint.device_id}),
            ),
            None => (
                "/wham/remote/control/server/enroll",
                serde_json::json!({"name":endpoint.display_name,
                "os":if cfg!(target_os="macos"){"darwin"}else{std::env::consts::OS},"arch":device_arch(),
                "app_server_version":version,"installation_id":endpoint.device_id}),
            ),
        };
        let body = serde_json::to_vec(&body).map_err(|_| ControlError::Invalid)?;
        let session: HostSession = self
            .account_request(
                auth,
                endpoint.tenant_id,
                &endpoint.account,
                ControlRequest {
                    method: reqwest::Method::POST,
                    path,
                    body: Some(&body),
                    installation: Some(endpoint.device_id),
                    refresh_start: false,
                },
            )
            .await
            .map_err(|error| match error {
                ControlError::Rejected(404) if previous.is_some() => {
                    ControlError::HostRegistrationMissing
                }
                error => error,
            })?;
        session.validate(previous)?;
        Ok(session)
    }
    pub(crate) async fn pair_host(
        &self,
        session: &HostSession,
    ) -> Result<PairingCode, ControlError> {
        session.validate(None)?;
        let response = self
            .client()
            .post(format!("{}/wham/remote/control/server/pair", self.api_base))
            .header("originator", "codex_rc_bridge")
            .bearer_auth(session.remote_control_token.expose())
            .json(&serde_json::json!({"manual_code":true}))
            .send()
            .await
            .map_err(|_| AuthError::Network)?;
        if !response.status().is_success() {
            return Err(ControlError::Rejected(response.status().as_u16()));
        }
        let pairing: PairingCode = decode(response).await?;
        if pairing.server_id != session.server_id
            || pairing.environment_id != session.environment_id
            || pairing.expires_unix()? <= unix_time()
        {
            return Err(ControlError::Invalid);
        }
        normalize_pairing_code(&pairing.manual_pairing_code)?;
        Ok(pairing)
    }
    pub async fn pair_controller<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        registration: &Registration,
        code: &SecretString,
    ) -> Result<(), ControlError> {
        let code = normalize_pairing_code(code)?;
        #[derive(Serialize)]
        struct Pair<'a> {
            client_id: &'a str,
            manual_pairing_code: &'a str,
        }
        let body = Zeroizing::new(
            serde_json::to_vec(&Pair {
                client_id: &registration.client_id,
                manual_pairing_code: &code,
            })
            .map_err(|_| ControlError::Invalid)?,
        );
        let _: serde_json::Value = self
            .control(
                auth,
                tenant,
                account,
                "/wham/remote/control/client/pair",
                &body,
                false,
            )
            .await?;
        Ok(())
    }
    pub async fn environments<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        client: &str,
    ) -> Result<Vec<Environment>, ControlError> {
        let mut result = Vec::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let mut url =
                url::Url::parse("https://chatgpt.com/backend-api/codex/remote/control/clients/")
                    .map_err(|_| ControlError::Invalid)?;
            url.path_segments_mut()
                .map_err(|_| ControlError::Invalid)?
                .pop_if_empty()
                .push(client)
                .push("environments");
            url.query_pairs_mut().append_pair("limit", "100");
            if let Some(cursor) = cursor.as_deref() {
                url.query_pairs_mut().append_pair("cursor", cursor);
            }
            let path = url
                .as_str()
                .strip_prefix("https://chatgpt.com/backend-api")
                .ok_or(ControlError::Invalid)?;
            let page: EnvironmentPage = self
                .account_request(
                    auth,
                    tenant,
                    account,
                    ControlRequest {
                        method: reqwest::Method::GET,
                        path,
                        body: None,
                        installation: None,
                        refresh_start: false,
                    },
                )
                .await?;
            if result.len() + page.items.len() > 10000 {
                return Err(ControlError::Invalid);
            }
            result.extend(page.items);
            match page.cursor {
                None => return Ok(result),
                Some(next) if next.is_empty() => return Ok(result),
                Some(next) => {
                    if next.len() > 8192 || !seen.insert(next.clone()) {
                        return Err(ControlError::Invalid);
                    }
                    cursor = Some(next);
                }
            }
        }
        Err(ControlError::Invalid)
    }
    pub async fn target_online<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        client: &str,
        target: &str,
    ) -> Result<Option<bool>, ControlError> {
        Ok(self
            .target_status(auth, tenant, account, client, target)
            .await?
            .online())
    }
    /// Computer metadata and access anomalies reported by the same complete listing.
    pub(crate) async fn target_status<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        client: &str,
        target: &str,
    ) -> Result<crate::output_sync::TargetObservation, ControlError> {
        crate::output_sync::TargetObservation::from_environments(
            &self.environments(auth, tenant, account, client).await?,
            target,
        )
    }
}
pub(crate) fn normalize_pairing_code(
    code: &SecretString,
) -> Result<Zeroizing<String>, ControlError> {
    let normalized = Zeroizing::new(
        code.expose()
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect::<String>()
            .to_ascii_uppercase(),
    );
    if normalized.len() != 8 || !normalized.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return Err(ControlError::Invalid);
    }
    Ok(Zeroizing::new(format!(
        "{}-{}",
        &normalized[..4],
        &normalized[4..]
    )))
}
impl TokenEndpoint for Upstream {
    async fn token(&self, form: &[(&str, &str)]) -> Result<TokenResponse, AuthError> {
        let response = self
            .client()
            .post(&self.token_url)
            .form(form)
            .send()
            .await
            .map_err(|_| AuthError::Network)?;
        if !response.status().is_success() {
            tracing::warn!(
                event = "oauth.token_rejected",
                status = response.status().as_u16()
            );
            // Gate on status before reading OAuth errors: rate limits/timeouts/5xx stay transient.
            let oauth_rejection = response.status().is_client_error()
                && !matches!(response.status().as_u16(), 408 | 429);
            let body: serde_json::Value = decode(response).await.unwrap_or_default();
            let oauth_error = body.get("error").and_then(|v| v.as_str());
            let code = oauth_error
                .or_else(|| {
                    body.get("error")
                        .and_then(|v| v.get("code"))
                        .and_then(|v| v.as_str())
                })
                .or_else(|| body.get("code").and_then(|v| v.as_str()));
            return Err(match (oauth_rejection, code, oauth_error) {
                (
                    true,
                    Some(
                        "invalid_grant"
                        | "refresh_token_expired"
                        | "refresh_token_reused"
                        | "refresh_token_invalidated",
                    ),
                    _,
                ) => AuthError::Rejected,
                // A plain code field is also common in gateway responses. Only the
                // known credential failures above qualify without OAuth's string `error`.
                (true, _, Some(_)) => AuthError::OAuthRequest,
                _ => AuthError::Network,
            });
        }
        decode(response).await
    }
}
async fn decode<T: DeserializeOwned>(mut response: Response) -> Result<T, AuthError> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE as u64)
    {
        return Err(AuthError::Response);
    }
    let mut raw = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Network)? {
        if raw.len() + chunk.len() > MAX_RESPONSE {
            return Err(AuthError::Response);
        }
        raw.extend_from_slice(&chunk);
    }
    serde_json::from_slice(if raw.is_empty() { b"{}" } else { &raw })
        .map_err(|_| AuthError::Response)
}
