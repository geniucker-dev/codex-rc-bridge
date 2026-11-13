use bridge_core::{
    auth::{AuthService, SecretString},
    chains::{ChainController, ChainCoordinator, ChainError, RuntimeFactory},
    configuration::{Command, Reply},
    deployment,
    model::{AccountIdentity, EndpointKind},
    storage::SqliteStore,
    upstream::{Upstream, unix_time},
    vault::{EncryptionKey, SecretScope, Vault},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    sync::{Mutex, RwLock},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub status: u16,
    pub code: Option<&'static str>,
    pub message: String,
}
impl Error {
    pub fn local(message: &str) -> Self {
        Self {
            status: 409,
            code: None,
            message: message.into(),
        }
    }
    fn invalid() -> Self {
        Self::local("The submitted information is incomplete or invalid.")
    }
}
impl From<bridge_core::operation_error::OperationError> for Error {
    fn from(error: bridge_core::operation_error::OperationError) -> Self {
        Self {
            status: error.status,
            code: Some(error.code),
            message: "The operation did not complete.".into(),
        }
    }
}
impl From<ChainError> for Error {
    fn from(error: ChainError) -> Self {
        bridge_core::operation_error::OperationError::from(error).into()
    }
}
fn body<T: DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::invalid())
}
fn value(v: impl Serialize) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(|_| Error::invalid())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountLogin {
    expected: Option<AccountIdentity>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountRequest {
    account: AccountIdentity,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewEndpoint {
    kind: EndpointKind,
    account: AccountIdentity,
    name: String,
    output: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Callback {
    callback: SecretString,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Code {
    code: SecretString,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    output: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Network {
    #[serde(deserialize_with = "Option::deserialize")]
    proxy_url: Option<SecretString>,
}
struct Pending {
    transaction: Uuid,
    port: u16,
    expires: i64,
}

enum LoginStatus {
    Pending(i64),
    Processing,
    Finished {
        retain_until: i64,
        result: Result<(), Error>,
    },
}

pub struct DesktopService {
    pub controller: ChainController,
    pub tenant: Uuid,
    pub stop: CancellationToken,
    vault: Vault,
    upstream: Upstream,
    gate: RwLock<()>,
    callbacks: Mutex<HashMap<String, Pending>>,
    login_results: Mutex<HashMap<Uuid, LoginStatus>>,
    callback_task: Mutex<Option<(u16, JoinHandle<()>)>>,
    worker: Mutex<Option<JoinHandle<Result<(), ChainError>>>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreRecord {
    client_id: String,
}
impl DesktopService {
    pub async fn open(store: Arc<SqliteStore>, key: EncryptionKey) -> Result<Arc<Self>, Error> {
        let vault = Vault::new(store.clone(), key);
        let tenants = store
            .local_tenants()
            .await
            .map_err(|_| Error::local("Cannot read desktop state."))?;
        let tenant = match tenants.as_slice() {
            [] => {
                let tenant = Uuid::new_v4();
                vault.create_tenant_with_seed(tenant).await.map_err(|_| {
                    Error::local(
                        "Cannot create the local device identity. Check the disk and retry.",
                    )
                })?;
                tenant
            }
            [tenant] => {
                let seed = vault
                    .read(*tenant, &SecretScope::TenantSeed)
                    .await
                    .map_err(|_| {
                        Error::local("The Keychain key cannot decrypt the device identity.")
                    })?;
                if seed.as_ref().map(|bytes| bytes.len()) != Some(32) {
                    return Err(Error::local(
                        "The original device identity is missing. Restore the database and original Keychain key.",
                    ));
                }
                *tenant
            }
            _ => {
                return Err(Error::local(
                    "The desktop state directory must contain exactly one local user.",
                ));
            }
        };
        let proxy = deployment::load(&vault)
            .await
            .map_err(|_| Error::local("Cannot read proxy configuration."))?
            .flatten();
        let upstream = Upstream::new(proxy.as_ref().map(SecretString::expose))
            .map_err(|_| Error::local("Invalid proxy configuration."))?;
        let auth = Arc::new(AuthService::new(vault.clone(), upstream.clone()));
        let stop = CancellationToken::new();
        let (controller, worker) = ChainCoordinator::spawn(
            store.clone(),
            RuntimeFactory {
                store,
                vault: vault.clone(),
                auth,
                upstream: upstream.clone(),
            },
            tenant,
            stop.clone(),
        );
        Ok(Arc::new(Self {
            controller,
            tenant,
            stop,
            vault,
            upstream,
            gate: RwLock::new(()),
            callbacks: Mutex::new(HashMap::new()),
            login_results: Mutex::new(HashMap::new()),
            callback_task: Mutex::new(None),
            worker: Mutex::new(Some(worker)),
        }))
    }
    pub async fn shutdown(&self) -> Result<(), Error> {
        // Drain already accepted commands/network changes before stopping the coordinator.
        let _gate = self.gate.write().await;
        self.stop.cancel();
        if let Some((_, task)) = self.callback_task.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(worker) = self.worker.lock().await.take() {
            worker
                .await
                .map_err(|_| Error::local("The bridge could not shut down cleanly."))??;
        }
        Ok(())
    }
    fn network_view(&self, configured: bool) -> Value {
        let mut address = None;
        let mut authenticated = false;
        if let Some(raw) = self.upstream.proxy_url()
            && let Ok(mut url) = url::Url::parse(&raw)
        {
            authenticated = !url.username().is_empty() || url.password().is_some();
            let _ = url.set_username("");
            let _ = url.set_password(None);
            address = Some(url.to_string());
        }
        json!({"configured":configured,"mode":if address.is_some(){"proxy"}else{"direct"},"address":address,"authenticated":authenticated})
    }
    pub async fn request(
        self: &Arc<Self>,
        path: String,
        method: String,
        payload: Value,
    ) -> Result<Value, Error> {
        if self.stop.is_cancelled() {
            return Err(Error::local("The application is exiting."));
        }
        if path == "/deployment/network" && method == "POST" {
            let data: Network = body(payload)?;
            let prepared = Upstream::new(data.proxy_url.as_ref().map(SecretString::expose))
                .map_err(|_| Error::local("Invalid proxy address. Use http://host:port."))?;
            let _gate = self.gate.write().await;
            if self.stop.is_cancelled() {
                return Err(Error::local("The application is exiting."));
            }
            self.controller.pause_network().await?;
            if deployment::save(&self.vault, data.proxy_url.as_ref())
                .await
                .is_err()
            {
                let _ = self.controller.resume_network().await;
                return Err(Error::local("Cannot save proxy configuration."));
            }
            self.upstream.replace_network(&prepared);
            let failed = self.controller.resume_network().await.is_err();
            let mut result = self.network_view(true);
            result["restart_failures"] = json!(usize::from(failed));
            return Ok(result);
        }
        let _gate = self.gate.read().await;
        if self.stop.is_cancelled() {
            return Err(Error::local("The application is exiting."));
        }
        let pieces: Vec<_> = path.trim_start_matches('/').split('/').collect();
        let id = || {
            pieces
                .get(1)
                .and_then(|s| s.parse::<Uuid>().ok())
                .ok_or_else(Error::invalid)
        };
        let mut command = match (method.as_str(), pieces.as_slice()) {
            ("GET", ["capabilities"]) => {
                return Ok(json!({"management":true,"remoteControl":true,"desktop":true}));
            }
            ("GET", ["session"]) => {
                return Ok(
                    json!({"identity":{"username":"Local","tenant_id":self.tenant,"expires_at":0,"deployment_admin":true},"csrf":""}),
                );
            }
            ("GET", ["state"]) => {
                return Ok(
                    json!({"configuration":self.controller.configure(Command::Snapshot).await?,"chains":self.controller.status().await?}),
                );
            }
            ("GET", ["deployment", "network"]) => {
                return Ok(self.network_view(
                    deployment::load(&self.vault)
                        .await
                        .map_err(|_| Error::local("Cannot read proxy configuration."))?
                        .is_some(),
                ));
            }
            ("POST", ["accounts", "login"]) => Command::BeginAccountLogin {
                port: 1455,
                expected: body::<AccountLogin>(payload)?.expected,
            },
            ("POST", ["accounts", "check"]) => Command::CheckAccount {
                account: body::<AccountRequest>(payload)?.account,
            },
            ("GET", ["logins", _]) => {
                return Ok(self.login_status(id()?).await);
            }
            ("POST", ["logins", _]) => {
                let id = id()?;
                let callback = body::<Callback>(payload)?.callback;
                // Same lock order as the HTTP callback: exactly one completion owner.
                let mut callbacks = self.callbacks.lock().await;
                let mut results = self.login_results.lock().await;
                if matches!(results.get(&id), Some(LoginStatus::Processing)) {
                    return Err(Error::local(
                        "Authorization is being saved. Wait for the result.",
                    ));
                }
                if matches!(
                    results.get(&id),
                    Some(LoginStatus::Finished { result: Ok(()), .. })
                ) {
                    return Ok(Value::Null);
                }
                callbacks.retain(|_, pending| pending.transaction != id);
                results.insert(id, LoginStatus::Processing);
                Command::CompleteLogin {
                    transaction: id,
                    callback,
                }
            }
            ("DELETE", ["logins", _]) => {
                let id = id()?;
                let mut callbacks = self.callbacks.lock().await;
                let mut results = self.login_results.lock().await;
                if matches!(results.get(&id), Some(LoginStatus::Processing)) {
                    return Err(Error::local(
                        "Authorization is being saved. Wait for the result.",
                    ));
                }
                callbacks.retain(|_, pending| pending.transaction != id);
                results.remove(&id);
                Command::CancelLogin { transaction: id }
            }
            ("POST", ["endpoints"]) => {
                let e: NewEndpoint = body(payload)?;
                match e.kind {
                    EndpointKind::In => Command::CreateIn {
                        account: e.account,
                        name: e.name,
                        output: e.output,
                    },
                    EndpointKind::Out => Command::CreateOut {
                        account: e.account,
                        name: e.name,
                    },
                }
            }
            ("DELETE", ["endpoints", _]) => return value(self.controller.delete(id()?).await?),
            ("POST", ["outputs", _, "authorize"]) => Command::BeginOutLogin {
                port: 1455,
                output: id()?,
            },
            ("POST", ["outputs", _, "recover"]) => Command::RecoverOut { output: id()? },
            ("POST", ["outputs", _, "restore-record"]) => Command::RestoreOutRecord {
                output: id()?,
                client_id: body::<RestoreRecord>(payload)?.client_id,
            },
            ("POST", ["outputs", _, "pair"]) => Command::PairOut {
                output: id()?,
                code: body::<Code>(payload)?.code,
            },
            ("POST", ["inputs", _, "pair"]) => Command::PairIn { input: id()? },
            ("POST", ["inputs", _, "device", "sync"]) => {
                self.controller.sync_input_device(id()?).await?;
                return Ok(Value::Null);
            }
            ("POST", ["outputs", _, "device", "sync"]) => {
                self.controller.sync_output_device(id()?).await?;
                return Ok(Value::Null);
            }
            ("POST", ["inputs", _, "binding"]) => Command::Bind {
                input: id()?,
                output: body::<Binding>(payload)?.output,
            },
            ("DELETE", ["inputs", _, "binding"]) => {
                self.controller.unbind(id()?).await?;
                return Ok(Value::Null);
            }
            ("POST", ["inputs", _, "start"]) => {
                self.controller.start(id()?).await?;
                return Ok(Value::Null);
            }
            ("POST", ["inputs", _, "stop"]) => {
                self.controller.stop(id()?).await?;
                return Ok(Value::Null);
            }
            ("POST", ["inputs", _, "switch"]) => {
                self.controller
                    .switch(id()?, body::<Binding>(payload)?.output)
                    .await?;
                return Ok(Value::Null);
            }
            _ => return Err(Error::invalid()),
        };
        // Keep the listener reserved until its transaction is visible to idle cleanup.
        let mut callback_listener = match &mut command {
            Command::BeginAccountLogin { port, .. } | Command::BeginOutLogin { port, .. } => {
                let mut listener = self.callback_task.lock().await;
                *port = self
                    .start_callback(&mut listener, &[1455, 1457])
                    .await
                    .unwrap_or(1455);
                Some(listener)
            }
            _ => None,
        };
        let completing = matches!(&command, Command::CompleteLogin { .. });
        let reply = self.controller.configure(command).await.map_err(|error| {
            if matches!(pieces.as_slice(), ["outputs", _, "restore-record"]) {
                Error::from(bridge_core::operation_error::restore_record(error))
            } else {
                Error::from(error)
            }
        });
        if completing {
            self.finish_login(id()?, reply.as_ref().map(|_| ()).map_err(Clone::clone))
                .await;
        }
        let reply = reply?;
        if let Reply::Login(login) = &reply {
            let url = url::Url::parse(&login.url).map_err(|_| Error::invalid())?;
            let state = url
                .query_pairs()
                .find(|(k, _)| k == "state")
                .ok_or_else(Error::invalid)?
                .1
                .into_owned();
            let mut callbacks = self.callbacks.lock().await;
            callbacks.retain(|_, p| p.expires > unix_time());
            callbacks.insert(
                state,
                Pending {
                    transaction: login.id,
                    port: url
                        .query_pairs()
                        .find(|(key, _)| key == "redirect_uri")
                        .and_then(|(_, value)| url::Url::parse(&value).ok())
                        .and_then(|url| url.port())
                        .ok_or_else(Error::invalid)?,
                    expires: login.expires_at,
                },
            );
            drop(callbacks);
            self.login_results
                .lock()
                .await
                .insert(login.id, LoginStatus::Pending(login.expires_at));
            let automatic = callback_listener
                .as_ref()
                .is_some_and(|listener| listener.is_some());
            callback_listener.take();
            let mut reply = value(&reply)?;
            reply["value"]["automatic_callback"] = json!(automatic);
            return Ok(reply);
        }
        value(reply)
    }
    async fn login_status(&self, id: Uuid) -> Value {
        let now = unix_time();
        let mut results = self.login_results.lock().await;
        results.retain(|_, status| match status {
            LoginStatus::Pending(expires) => *expires > now,
            LoginStatus::Finished { retain_until, .. } => *retain_until > now,
            LoginStatus::Processing => true,
        });
        match results.get(&id) {
            Some(LoginStatus::Pending(_)) => json!({"status":"pending"}),
            Some(LoginStatus::Processing) => json!({"status":"processing"}),
            Some(LoginStatus::Finished { result: Ok(()), .. }) => json!({"status":"complete"}),
            Some(LoginStatus::Finished {
                result: Err(error), ..
            }) => json!({"status":"failed", "error":error}),
            None => json!({"status":"expired"}),
        }
    }
    async fn finish_login(&self, id: Uuid, result: Result<(), Error>) {
        self.login_results.lock().await.insert(
            id,
            LoginStatus::Finished {
                retain_until: unix_time() + 600,
                result,
            },
        );
    }
    async fn start_callback(
        self: &Arc<Self>,
        task: &mut Option<(u16, JoinHandle<()>)>,
        ports: &[u16],
    ) -> Option<u16> {
        if let Some((port, handle)) = task.as_ref()
            && !handle.is_finished()
        {
            return Some(*port);
        }
        task.take();
        let mut bound = None;
        for port in ports {
            match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, *port)).await {
                Ok(listener) => {
                    bound = Some((*port, listener));
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(_) => return None,
            }
        }
        let (port, listener) = bound?;
        let state = self.clone();
        let router = axum::Router::new()
            .route("/auth/callback", axum::routing::get(callback))
            .with_state(state);
        let idle = Arc::downgrade(self);
        *task = Some((
            port,
            tokio::spawn(async move {
                let shutdown = async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        let Some(service) = idle.upgrade() else {
                            break;
                        };
                        // Serialize the idle decision with a new listener request. A task
                        // that has begun shutting down must not advertise a live callback.
                        let mut task = service.callback_task.lock().await;
                        let mut pending = service.callbacks.lock().await;
                        pending.retain(|_, p| p.expires > unix_time());
                        if pending.is_empty() || service.stop.is_cancelled() {
                            task.take();
                            break;
                        }
                    }
                };
                let _ = axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown)
                    .await;
            }),
        ));
        Some(port)
    }
}
async fn callback(
    axum::extract::State(service): axum::extract::State<Arc<DesktopService>>,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
) -> (
    axum::http::StatusCode,
    [(axum::http::HeaderName, &'static str); 3],
    &'static str,
) {
    use axum::http::{StatusCode, header};
    let security = [
        (header::CACHE_CONTROL, "no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; frame-ancestors 'none'",
        ),
    ];
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    if !matches!(host, Some("localhost:1455" | "localhost:1457")) {
        return (StatusCode::BAD_REQUEST, security, "Invalid callback host");
    }
    let raw = format!("http://{}{uri}", host.unwrap());
    let Ok(url) = url::Url::parse(&raw) else {
        return (StatusCode::BAD_REQUEST, security, "Invalid callback");
    };
    let states: Vec<_> = url.query_pairs().filter(|(k, _)| k == "state").collect();
    if states.len() != 1 || raw.len() > 16384 {
        return (StatusCode::BAD_REQUEST, security, "Invalid callback");
    }
    let mut callbacks = service.callbacks.lock().await;
    if callbacks
        .get(states[0].1.as_ref())
        .is_some_and(|pending| host != Some(format!("localhost:{}", pending.port).as_str()))
    {
        return (StatusCode::BAD_REQUEST, security, "Invalid callback host");
    }
    let pending = callbacks.remove(states[0].1.as_ref());
    let Some(pending) = pending.filter(|p| p.expires > unix_time()) else {
        return (
            StatusCode::BAD_REQUEST,
            security,
            "Authorization expired or already completed. Return to Codex RC Bridge.",
        );
    };
    let id = pending.transaction;
    service
        .login_results
        .lock()
        .await
        .insert(id, LoginStatus::Processing);
    drop(callbacks);
    // Keep completion alive if the browser closes the response connection.
    tokio::spawn(async move {
        let _gate = service.gate.read().await;
        let result = if service.stop.is_cancelled() {
            Err(Error::local("The application is exiting."))
        } else {
            service
                .controller
                .configure(Command::CompleteLogin {
                    transaction: id,
                    callback: SecretString::new(raw),
                })
                .await
                .map(|_| ())
                .map_err(Error::from)
        };
        service.finish_login(id, result).await;
    });
    (
        StatusCode::OK,
        security,
        "Authorization received. Return to Codex RC Bridge to view the result.",
    )
}

#[cfg(test)]
mod tests;
