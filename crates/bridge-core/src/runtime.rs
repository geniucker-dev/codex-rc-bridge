//! One fixed IN/OUT chain. Tasks share account refresh and join before shutdown completes.
pub mod status;
use crate::{
    auth::{AccountTokens, AuthService},
    delivery::{Delivery, Limits},
    device::{self, ControllerSession, Registration},
    identity::{self, OutSigningKey},
    model::{Binding, Endpoint, EndpointKind, Target},
    presence::Presence,
    routing::Router,
    storage::SqliteStore,
    transport::{self, TransportError},
    upstream::{ControlError, HostRegistration, HostSession, Upstream, unix_time},
    vault::{SecretScope, Vault},
    wire::Side,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use status::{ConnectionReporter, ConnectionStatus, FaultSide};
use std::sync::Arc;
use tokio::{
    sync::{Mutex, watch},
    task::JoinSet,
    time::{Duration, Instant, sleep, timeout},
};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("{source}")]
    At {
        side: FaultSide,
        source: Box<RuntimeError>,
    },
    #[error("chain configuration or protected state is invalid")]
    State,
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("bridge worker failed")]
    Worker,
}
impl RuntimeError {
    pub fn at(self, side: FaultSide) -> Self {
        Self::At {
            side,
            source: Box::new(self),
        }
    }
    pub fn root(&self) -> &Self {
        match self {
            Self::At { source, .. } => source.root(),
            error => error,
        }
    }
    pub fn retryable(&self) -> bool {
        match self.root() {
            Self::Control(error) => error.retryable(),
            Self::Transport(error) => error.retryable(),
            _ => false,
        }
    }
}
struct Bundle {
    host: HostSession,
    controller: ControllerSession,
    account_b: AccountTokens,
    deadline: i64,
    created: Instant,
}
#[derive(Default)]
struct Sessions {
    current: Option<Arc<Bundle>>,
    used: bool,
}
struct Context {
    input: Endpoint,
    output: Endpoint,
    target: Target,
    host: HostRegistration,
    controller: Registration,
    key: OutSigningKey,
    auth: Arc<AuthService<Upstream>>,
    upstream: Upstream,
    sessions: Mutex<Sessions>,
    presence: watch::Sender<Presence>,
    status: ConnectionReporter,
    records: Arc<SqliteStore>,
}
pub struct BridgeRuntime {
    context: Arc<Context>,
    delivery: Arc<Mutex<Delivery>>,
}
impl BridgeRuntime {
    pub async fn load(
        store: Arc<SqliteStore>,
        vault: Vault,
        auth: Arc<AuthService<Upstream>>,
        upstream: Upstream,
        tenant: Uuid,
        input: Uuid,
    ) -> Result<Self, RuntimeError> {
        if store
            .switch_pending(tenant, input)
            .await
            .map_err(|_| RuntimeError::State)?
        {
            return Err(RuntimeError::State);
        }
        let binding = store
            .binding(tenant, input)
            .await
            .map_err(|_| RuntimeError::State)?
            .ok_or(RuntimeError::State)?;
        Self::load_binding(store, vault, auth, upstream, binding).await
    }
    pub(crate) async fn load_binding(
        store: Arc<SqliteStore>,
        vault: Vault,
        auth: Arc<AuthService<Upstream>>,
        upstream: Upstream,
        binding: Binding,
    ) -> Result<Self, RuntimeError> {
        let tenant = binding.tenant_id;
        let input = binding.in_id;
        let input = store
            .endpoint(tenant, input)
            .await
            .map_err(|_| RuntimeError::State)?
            .ok_or(RuntimeError::State)?;
        let output = store
            .endpoint(tenant, binding.out_id)
            .await
            .map_err(|_| RuntimeError::State)?
            .ok_or(RuntimeError::State)?;
        if input.kind != EndpointKind::In || output.kind != EndpointKind::Out {
            return Err(RuntimeError::State);
        }
        let active:i64=sqlx::query_scalar("SELECT count(*) FROM endpoints WHERE tenant_id=? AND device_id IN (?,?) AND lifecycle='active'")
            .bind(tenant.to_string()).bind(input.device_id.to_string()).bind(output.device_id.to_string()).fetch_one(&store.pool).await.map_err(|_|RuntimeError::State)?;
        if active != 2
            || store
                .output_pairing_pending(tenant, output.device_id)
                .await
                .map_err(|_| RuntimeError::State)?
        {
            return Err(RuntimeError::State);
        }
        let target = store
            .target(tenant, output.device_id)
            .await
            .map_err(|_| RuntimeError::State)?
            .ok_or(RuntimeError::State)?;
        if store
            .is_local_environment(&target.environment_id)
            .await
            .map_err(|_| RuntimeError::State)?
        {
            return Err(RuntimeError::State);
        }
        for (endpoint, side) in [
            (&input, FaultSide::Controller),
            (&output, FaultSide::Device),
        ] {
            if store
                .sign_in_required(tenant, &endpoint.account)
                .await
                .map_err(|_| RuntimeError::State)?
            {
                return Err(RuntimeError::Control(ControlError::Auth(
                    crate::auth::AuthError::SignInRequired,
                ))
                .at(side));
            }
        }
        let host = load_registration(&vault, tenant, SecretScope::InRegistration(input.device_id))
            .await
            .map_err(|error| error.at(FaultSide::Controller))?;
        let controller: Registration = load_registration(
            &vault,
            tenant,
            SecretScope::OutRegistration(output.device_id),
        )
        .await
        .map_err(|error| error.at(FaultSide::Device))?;
        let seed = vault
            .tenant_seed(tenant)
            .await
            .map_err(|_| RuntimeError::State)?;
        let key = identity::signing_key(&seed, tenant, &output.account, &output.identity_name)
            .map_err(|_| RuntimeError::State)?;
        if controller.key_id != device::identity(&key).key_id {
            return Err(RuntimeError::State);
        }
        let records = store.clone();
        let delivery = Delivery::new(
            Router::new(
                store,
                tenant,
                input.device_id,
                controller.client_id.clone(),
                target.environment_id.clone(),
            ),
            Limits::default(),
        );
        let (presence, _) = watch::channel(Presence::default());
        Ok(Self {
            context: Arc::new(Context {
                input,
                output,
                target,
                host,
                controller,
                key,
                auth,
                upstream,
                sessions: Mutex::new(Sessions::default()),
                presence,
                status: ConnectionReporter::default(),
                records,
            }),
            delivery: Arc::new(Mutex::new(delivery)),
        })
    }
    pub(crate) fn input(&self) -> &Endpoint {
        &self.context.input
    }
    pub(crate) fn host_server_id(&self) -> &str {
        &self.context.host.server_id
    }
    pub(crate) fn target_version(&self) -> &str {
        &self.context.target.app_server_version
    }
    pub fn with_reporter(mut self, reporter: ConnectionReporter) -> Self {
        Arc::get_mut(&mut self.context)
            .expect("runtime is not started")
            .status = reporter;
        self
    }
    pub async fn run(self, stop: CancellationToken) -> Result<(), RuntimeError> {
        let cancel = stop.child_token();
        let mut tasks = JoinSet::new();
        tasks.spawn(connections(
            self.context.clone(),
            self.delivery.clone(),
            cancel.clone(),
        ));
        tasks.spawn(probes(self.context.clone(), cancel.clone()));
        tasks.spawn(renewals(self.context.clone(), cancel.clone()));
        let first = tokio::select! {biased;_=stop.cancelled()=>Ok(()),result=tasks.join_next()=>match result {Some(Ok(result))=>result,_=>Err(RuntimeError::Worker)}};
        cancel.cancel();
        self.context.status.finish(&first);
        // HTTP refreshes finish their credential commit before worker exit. No detached refresh writes.
        let mut outcome = first;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Err(error)) if outcome.is_ok() => outcome = Err(error),
                Err(_) if outcome.is_ok() => outcome = Err(RuntimeError::Worker),
                _ => {}
            }
        }
        self.delivery
            .lock()
            .await
            .retire_all()
            .map_err(TransportError::from)?;
        tracing::info!(event = "bridge.stopped");
        outcome
    }
}
pub async fn load_registration<T: serde::de::DeserializeOwned>(
    vault: &Vault,
    tenant: Uuid,
    scope: SecretScope,
) -> Result<T, RuntimeError> {
    let missing = match &scope {
        SecretScope::InRegistration(_) => ControlError::HostRegistrationMissing,
        SecretScope::OutRegistration(_) => ControlError::Registration,
        _ => return Err(RuntimeError::State),
    };
    let raw = vault
        .read(tenant, &scope)
        .await
        .map_err(|_| RuntimeError::State)?
        .ok_or(RuntimeError::Control(missing))?;
    serde_json::from_slice(&raw).map_err(|_| RuntimeError::State)
}
impl Context {
    async fn credentials(&self, connection: bool) -> Result<Arc<Bundle>, RuntimeError> {
        let mut sessions = self.sessions.lock().await;
        let needs = sessions.current.as_ref().is_none_or(|b| {
            b.deadline <= unix_time()
                || (connection && (sessions.used || b.created.elapsed() >= Duration::from_secs(60)))
        });
        if needs {
            let tenant = self.input.tenant_id;
            let controller = self
                .upstream
                .refresh_controller(
                    &self.auth,
                    tenant,
                    &self.output.account,
                    &self.key,
                    &self.controller,
                )
                .await
                .map_err(|e| RuntimeError::from(e).at(FaultSide::Device))?;
            let host = self
                .upstream
                .host_session(
                    &self.auth,
                    &self.input,
                    Some(&self.host),
                    &self.target.app_server_version,
                )
                .await
                .map_err(|e| RuntimeError::from(e).at(FaultSide::Controller))?;
            let (a, b) = tokio::join!(
                self.auth
                    .account(tenant, &self.input.account, None, unix_time()),
                self.auth
                    .account(tenant, &self.output.account, None, unix_time())
            );
            let (account_a, account_b) = (
                a.map_err(|e| RuntimeError::from(ControlError::from(e)).at(FaultSide::Controller))?,
                b.map_err(|e| RuntimeError::from(ControlError::from(e)).at(FaultSide::Device))?,
            );
            let deadline = [
                host.expiry()? - 60,
                controller.expiry().map_err(ControlError::from)? - 60,
                account_a.expires_at as i64 - 60,
                account_b.expires_at as i64 - 60,
            ]
            .into_iter()
            .min()
            .ok_or(ControlError::Invalid)?;
            if deadline <= unix_time() {
                return Err(ControlError::Invalid.into());
            }
            sessions.current = Some(Arc::new(Bundle {
                host,
                controller,
                account_b,
                deadline,
                created: Instant::now(),
            }));
            sessions.used = false;
            tracing::info!(event = "credentials.refreshed");
        }
        if connection {
            sessions.used = true;
        }
        sessions
            .current
            .clone()
            .ok_or_else(|| ControlError::Invalid.into())
    }
}
async fn probes(context: Arc<Context>, stop: CancellationToken) -> Result<(), RuntimeError> {
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        let report = context
            .upstream
            .target_status(
                &context.auth,
                context.input.tenant_id,
                &context.output.account,
                &context.controller.client_id,
                &context.target.environment_id,
            )
            .await;
        if let Ok(observation) = &report {
            match context
                .records
                .observe_target(
                    context.input.tenant_id,
                    context.output.device_id,
                    &context.target.environment_id,
                    observation,
                )
                .await
            {
                Ok(true) => context.status.refresh(),
                Ok(false) => {}
                Err(_) => tracing::warn!(event = "target.cache_write_failed"),
            }
        }
        match report.map(|observation| observation.online()) {
            Ok(Some(online)) => {
                let changed = context.presence.borrow().online() != online;
                context
                    .presence
                    .send_modify(|p| p.report(online, Instant::now()));
                if changed {
                    tracing::info!(event = "target.presence", online);
                }
            }
            Ok(None) => {}
            Err(error) if error.retryable() => tracing::warn!(event = "target.probe_retry"),
            Err(error) => return Err(RuntimeError::from(error).at(FaultSide::Device)),
        }
        // Expiration also runs while a slow probe is in flight, in the connection task.
        tokio::select! {_=stop.cancelled()=>return Ok(()),_=sleep(Duration::from_secs(5))=>{}}
    }
}
async fn renewals(context: Arc<Context>, stop: CancellationToken) -> Result<(), RuntimeError> {
    let mut attempt = 0;
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        let wait = match context.credentials(false).await {
            Ok(bundle) => {
                attempt = 0;
                Duration::from_secs((bundle.deadline - unix_time()).max(1) as u64)
            }
            Err(error) if error.retryable() => {
                attempt += 1;
                tracing::warn!(event = "credentials.refresh_retry");
                backoff(attempt)
            }
            Err(error) => return Err(error),
        };
        tokio::select! {_=stop.cancelled()=>return Ok(()),_=sleep(wait)=>{}}
    }
}
pub(crate) fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(
        (1u64 << attempt.saturating_sub(1).min(5)).min(30) * 1000
            + (Uuid::new_v4().as_u128() % 1000) as u64,
    )
}
async fn connections(
    context: Arc<Context>,
    delivery: Arc<Mutex<Delivery>>,
    stop: CancellationToken,
) -> Result<(), RuntimeError> {
    let expire_context = context.clone();
    let expire_stop = stop.clone();
    let expire = async move {
        loop {
            tokio::select! {_=expire_stop.cancelled()=>break,_=sleep(Duration::from_secs(1))=>{expire_context.presence.send_modify(|p|p.expire(Instant::now()));}}
        }
    };
    let connect = connection_loop(context, delivery, stop);
    tokio::pin!(expire);
    tokio::pin!(connect);
    tokio::select! {result=&mut connect=>result,_=&mut expire=>connect.await}
}
async fn connection_loop(
    context: Arc<Context>,
    delivery: Arc<Mutex<Delivery>>,
    stop: CancellationToken,
) -> Result<(), RuntimeError> {
    let mut online = context.presence.subscribe();
    let mut attempt = 0;
    let mut auth_retried = false;
    let mut pending_auth: Option<crate::auth::SecretString> = None;
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        while !online.borrow_and_update().online() {
            context
                .status
                .report(if online.borrow().confirmed_offline() {
                    ConnectionStatus::TargetOffline
                } else {
                    ConnectionStatus::CheckingTarget
                });
            tokio::select! {_=stop.cancelled()=>return Ok(()),result=online.changed()=>{result.map_err(|_|RuntimeError::Worker)?;}}
        }
        context.status.report(if attempt == 0 && !auth_retried {
            ConnectionStatus::Connecting
        } else {
            ConnectionStatus::Reconnecting
        });
        if let Some(rejected) = &pending_auth {
            match context
                .auth
                .account(
                    context.input.tenant_id,
                    &context.output.account,
                    Some(rejected),
                    unix_time(),
                )
                .await
            {
                Ok(_) => pending_auth = None,
                Err(crate::auth::AuthError::Network) => {
                    context.status.report(ConnectionStatus::Reconnecting);
                    attempt += 1;
                    tokio::select! {_=stop.cancelled()=>return Ok(()),_=sleep(backoff(attempt))=>{}}
                    continue;
                }
                Err(error) => {
                    return Err(RuntimeError::from(ControlError::from(error)).at(FaultSide::Device));
                }
            }
        }
        let bundle = match context.credentials(true).await {
            Ok(bundle) => bundle,
            Err(error) if error.retryable() => {
                context.status.report(ConnectionStatus::Reconnecting);
                attempt += 1;
                tokio::select! {_=stop.cancelled()=>return Ok(()),_=sleep(backoff(attempt))=>{}}
                continue;
            }
            Err(error) => return Err(error),
        };
        let started = Instant::now();
        let (connected, connection) =
            connect_once(&context, &bundle, delivery.clone(), stop.clone()).await;
        if connected {
            auth_retried = false;
        }
        match connection {
            Ok(()) => return Ok(()),
            Err(TransportError::Offline) => continue,
            Err(TransportError::Handshake {
                side: Side::Out,
                status: 401,
            }) if !auth_retried => {
                auth_retried = true;
                pending_auth = Some(bundle.account_b.access_token().clone());
                continue;
            }
            Err(error) if !error.retryable() => return Err(error.into()),
            Err(_) => {}
        }
        if started.elapsed() > Duration::from_secs(30) {
            attempt = 0;
        }
        attempt += 1;
        context.status.report(ConnectionStatus::Reconnecting);
        tracing::info!(event = "bridge.reconnecting", attempt);
        tokio::select! {_=stop.cancelled()=>return Ok(()),_=sleep(backoff(attempt))=>{}}
    }
}
async fn connect_once(
    context: &Context,
    bundle: &Bundle,
    delivery: Arc<Mutex<Delivery>>,
    stop: CancellationToken,
) -> (bool, Result<(), TransportError>) {
    let mut out_headers = vec![
        (
            "authorization",
            format!("Bearer {}", bundle.account_b.access_token().expose()),
        ),
        (
            "ChatGPT-Account-Id",
            context.output.account.workspace_id.clone(),
        ),
        ("x-codex-client-id", context.controller.client_id.clone()),
        ("x-codex-protocol-version", "3".into()),
        (
            "x-codex-client-session-token",
            format!("Bearer {}", bundle.controller.remote_control_token.expose()),
        ),
    ];
    let mut in_headers = vec![
        (
            "authorization",
            format!("Bearer {}", bundle.host.remote_control_token.expose()),
        ),
        ("x-codex-server-id", bundle.host.server_id.clone()),
        (
            "x-codex-name",
            STANDARD.encode(context.input.display_name.as_bytes()),
        ),
        (
            "x-codex-installation-id",
            context.input.device_id.to_string(),
        ),
        ("x-codex-protocol-version", "3".into()),
    ];
    {
        let delivery = delivery.lock().await;
        for (side, headers) in [(Side::In, &mut in_headers), (Side::Out, &mut out_headers)] {
            if let Some(cursor) = delivery.subscribe_cursor(side) {
                headers.push(("x-codex-subscribe-cursor", cursor.to_owned()));
            }
        }
    }
    let opening = async {
        tracing::info!(event = "bridge.connecting");
        let mut output = transport::connect(
            Side::Out,
            &context.upstream.websocket_url(Side::Out),
            out_headers,
            context.upstream.proxy_url().as_deref(),
        )
        .await?;
        let proof = timeout(Duration::from_secs(10), async {
            loop {
                match output
                    .next()
                    .await
                    .ok_or(TransportError::Network)?
                    .map_err(|error| transport::socket_error(error, Side::Out))?
                {
                    Message::Text(text) => {
                        let challenge =
                            serde_json::from_str(&text).map_err(|_| TransportError::Invalid)?;
                        break device::connection_proof(
                            &context.key,
                            &context.controller,
                            &bundle.controller,
                            &challenge,
                            unix_time(),
                        )
                        .map_err(|_| TransportError::Invalid);
                    }
                    Message::Ping(data) => output
                        .send(Message::Pong(data))
                        .await
                        .map_err(|_| TransportError::Network)?,
                    Message::Pong(_) => {}
                    Message::Close(frame) => {
                        return Err(TransportError::Closed(
                            frame.map_or(1005, |f| f.code.into()),
                        ));
                    }
                    _ => return Err(TransportError::Invalid),
                }
            }
        })
        .await
        .map_err(|_| TransportError::Network)??;
        let raw = serde_json::to_string(&proof).map_err(|_| TransportError::Invalid)?;
        timeout(
            Duration::from_secs(10),
            output.send(Message::Text(raw.into())),
        )
        .await
        .map_err(|_| TransportError::Network)?
        .map_err(|_| TransportError::Network)?;
        if !context.presence.borrow().online() {
            return Err(TransportError::Offline);
        }
        let input = transport::connect(
            Side::In,
            &context.upstream.websocket_url(Side::In),
            in_headers,
            context.upstream.proxy_url().as_deref(),
        )
        .await?;
        if !context.presence.borrow().online() {
            return Err(TransportError::Offline);
        }
        Ok((input, output))
    };
    let mut online = context.presence.subscribe();
    let sockets = tokio::select! {biased;_=stop.cancelled()=>return (false,Ok(())),_=online.wait_for(|p|!p.online())=>Err(TransportError::Offline),result=opening=>result};
    match sockets {
        Ok((input, output)) => {
            context.status.report(ConnectionStatus::Connected);
            tracing::info!(event = "bridge.connected");
            (
                true,
                transport::relay(input, output, delivery, context.presence.clone(), stop).await,
            )
        }
        Err(error) => (false, Err(error)),
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
