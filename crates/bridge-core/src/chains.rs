//! Tenant-scoped chain ownership. The process adapter must hold the database lock.
//! Commands are owned by an actor: dropping an HTTP request cannot interrupt a cutover.
mod storage;

use crate::{
    auth::AuthService,
    lifecycle::{DeleteError, DeleteOutcome},
    model::{Binding, Endpoint},
    runtime::{
        BridgeRuntime, RuntimeError,
        status::{ConnectionReporter, ConnectionStatus, FaultSide, Observation},
    },
    storage::{SqliteStore, StorageError},
    upstream::Upstream,
    vault::Vault,
};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::Arc,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub type ChainTask = Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send>>;

/// Preparation validates local credentials/configuration without starting network work.
pub trait ChainFactory: Send + Sync + 'static {
    fn sync_output_device(
        &self,
        _tenant: Uuid,
        _output: Uuid,
    ) -> impl Future<Output = Result<bool, crate::configuration::ConfigurationError>> + Send {
        std::future::ready(Err(crate::configuration::ConfigurationError::State))
    }
    fn sync_input_device(
        &self,
        _tenant: Uuid,
        _input: Uuid,
    ) -> impl Future<Output = Result<bool, crate::configuration::ConfigurationError>> + Send {
        std::future::ready(Err(crate::configuration::ConfigurationError::State))
    }
    fn input_tracker(
        &self,
        _tenant: Uuid,
        _input: Uuid,
        _expires: i64,
        _stop: CancellationToken,
        _changes: watch::Sender<()>,
    ) -> Option<Pin<Box<dyn Future<Output = ()> + Send>>> {
        None
    }

    fn configure(
        &self,
        _tenant: Uuid,
        _command: crate::configuration::Command,
    ) -> impl Future<
        Output = Result<crate::configuration::Reply, crate::configuration::ConfigurationError>,
    > + Send {
        std::future::ready(Err(crate::configuration::ConfigurationError::State))
    }
    fn prepare(
        &self,
        binding: Binding,
        stop: CancellationToken,
    ) -> impl Future<Output = Result<ChainTask, RuntimeError>> + Send;
    fn prepare_observed(
        &self,
        binding: Binding,
        stop: CancellationToken,
        _reporter: ConnectionReporter,
    ) -> impl Future<Output = Result<ChainTask, RuntimeError>> + Send {
        self.prepare(binding, stop)
    }
    fn cleanup(
        &self,
        _endpoint: Endpoint,
    ) -> impl Future<Output = Result<DeleteOutcome, DeleteError>> + Send {
        async { Err(DeleteError::State) }
    }
}

#[derive(Clone)]
pub struct RuntimeFactory {
    pub store: Arc<SqliteStore>,
    pub vault: Vault,
    pub auth: Arc<AuthService<Upstream>>,
    pub upstream: Upstream,
}
impl ChainFactory for RuntimeFactory {
    async fn sync_output_device(
        &self,
        tenant: Uuid,
        output: Uuid,
    ) -> Result<bool, crate::configuration::ConfigurationError> {
        RuntimeFactory::sync_output_device(self, tenant, output).await
    }
    async fn sync_input_device(
        &self,
        tenant: Uuid,
        input: Uuid,
    ) -> Result<bool, crate::configuration::ConfigurationError> {
        RuntimeFactory::sync_input_device(self, tenant, input).await
    }
    fn input_tracker(
        &self,
        tenant: Uuid,
        input: Uuid,
        expires: i64,
        stop: CancellationToken,
        changes: watch::Sender<()>,
    ) -> Option<Pin<Box<dyn Future<Output = ()> + Send>>> {
        Some(Box::pin(crate::input_pairing::follow(
            self.clone(),
            tenant,
            input,
            expires,
            stop,
            changes,
        )))
    }

    async fn configure(
        &self,
        tenant: Uuid,
        command: crate::configuration::Command,
    ) -> Result<crate::configuration::Reply, crate::configuration::ConfigurationError> {
        RuntimeFactory::configure(self, tenant, command).await
    }
    async fn cleanup(&self, endpoint: Endpoint) -> Result<DeleteOutcome, DeleteError> {
        crate::lifecycle::cleanup(&self.vault, &self.auth, &self.upstream, &endpoint).await
    }
    async fn prepare(
        &self,
        binding: Binding,
        stop: CancellationToken,
    ) -> Result<ChainTask, RuntimeError> {
        self.prepare_observed(binding, stop, ConnectionReporter::default())
            .await
    }
    async fn prepare_observed(
        &self,
        binding: Binding,
        stop: CancellationToken,
        reporter: ConnectionReporter,
    ) -> Result<ChainTask, RuntimeError> {
        let runtime = BridgeRuntime::load_binding(
            self.store.clone(),
            self.vault.clone(),
            self.auth.clone(),
            self.upstream.clone(),
            binding.clone(),
        )
        .await?;
        let factory = self.clone();
        Ok(Box::pin(async move {
            let mut runtime = runtime;
            loop {
                let input = runtime.input().clone();
                let version = runtime.target_version().to_owned();
                let server = runtime.host_server_id().to_owned();
                let outcome = runtime
                    .with_reporter(reporter.clone())
                    .run(stop.clone())
                    .await;
                let stale_server = match outcome.as_ref().err().map(RuntimeError::root) {
                    Some(RuntimeError::Control(
                        crate::upstream::ControlError::HostRegistrationMissing,
                    )) => None,
                    Some(RuntimeError::Transport(
                        crate::transport::TransportError::HostRegistrationMissing,
                    )) => Some(server.as_str()),
                    _ => return outcome,
                };
                // run() has joined every worker and retired old streams before replacement.
                let mut attempt = 0u32;
                loop {
                    if stop.is_cancelled() {
                        return Ok(());
                    }
                    reporter.report(ConnectionStatus::Reconnecting);
                    match factory
                        .upstream
                        .registered_host_session(
                            &factory.auth,
                            &factory.vault,
                            &input,
                            &version,
                            stale_server,
                        )
                        .await
                    {
                        Ok(_) => break,
                        Err(error) if error.retryable() => {
                            attempt += 1;
                            tokio::select! {
                                _ = stop.cancelled() => return Ok(()),
                                _ = tokio::time::sleep(crate::runtime::backoff(attempt)) => {}
                            }
                        }
                        Err(error) => {
                            let result = Err(RuntimeError::from(error).at(FaultSide::Controller));
                            reporter.finish(&result);
                            return result;
                        }
                    }
                }
                if stop.is_cancelled() {
                    return Ok(());
                }
                runtime = BridgeRuntime::load_binding(
                    factory.store.clone(),
                    factory.vault.clone(),
                    factory.auth.clone(),
                    factory.upstream.clone(),
                    binding.clone(),
                )
                .await?;
            }
        }))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error(transparent)]
    Configuration(#[from] crate::configuration::ConfigurationError),
    #[error(transparent)]
    Deletion(#[from] DeleteError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("chain operation is unavailable or requires recovery")]
    Unavailable,
    #[error("tenant coordinator is not running")]
    Closed,
    #[error("chain worker did not shut down cleanly; restart required")]
    Worker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainStatus {
    Stopped,
    Running,
    Failed,
    Switching,
}
#[derive(Debug, serde::Serialize)]
pub struct ChainInfo {
    pub binding: Binding,
    /// Desired run state survives process restarts; it is not proof of B being online.
    pub enabled: bool,
    pub status: ChainStatus,
    pub connection: ConnectionStatus,
    pub side: Option<FaultSide>,
}
enum Repair {
    Account(crate::model::AccountIdentity),
    Device(Uuid),
    Controller(Uuid),
}

enum Operation {
    SyncOutputDevice(Uuid),
    SyncInputDevice(Uuid),
    PauseNetwork,
    ResumeNetwork,
    Configure(crate::configuration::Command),
    Delete(Uuid),
    Deleting,
    Start(Uuid),
    Stop(Uuid),
    Switch(Uuid, Uuid),
    Unbind(Uuid),
    Status,
}
struct Request {
    operation: Operation,
    reply: oneshot::Sender<Result<Reply, ChainError>>,
}
enum Reply {
    Configured(crate::configuration::Reply),
    Empty,
    Chains(Vec<ChainInfo>),
    Deleted(DeleteOutcome),
    Deleting(Vec<Endpoint>),
}
#[derive(Clone)]
pub struct ChainController {
    sender: mpsc::Sender<Request>,
    changes: watch::Receiver<()>,
    shutdown: CancellationToken,
}
pub struct ChainCoordinator;

impl ChainCoordinator {
    /// Exactly one coordinator per tenant, within the one database-owning process.
    /// Restores durable switch intents before starting any enabled chain.
    pub fn spawn<F: ChainFactory>(
        store: Arc<SqliteStore>,
        factory: F,
        tenant: Uuid,
        stop: CancellationToken,
    ) -> (ChainController, JoinHandle<Result<(), ChainError>>) {
        let (sender, receiver) = mpsc::channel(32);
        let (changes, subscription) = watch::channel(());
        let shutdown = stop.clone();
        let worker = tokio::spawn(async move {
            let mut actor = Actor {
                store,
                factory,
                tenant,
                running: HashMap::new(),
                uncertain: HashSet::new(),
                shutdown: stop.clone(),
                changes,
                failures: HashMap::new(),
                trackers: HashMap::new(),
            };
            let result = actor.serve(receiver, &stop).await;
            // All shutdown paths join every chain, including channel closure and failed recovery.
            for running in actor.running.values() {
                running.stop.cancel();
            }
            let mut cleanup = Ok(());
            for input in actor.running.keys().copied().collect::<Vec<_>>() {
                if let Err(error) = actor.join(input).await {
                    cleanup = Err(error);
                }
            }
            for input in actor.trackers.keys().copied().collect::<Vec<_>>() {
                if let Err(error) = actor.cancel_tracker(input).await {
                    cleanup = Err(error);
                }
            }
            result.and(cleanup)
        });
        (
            ChainController {
                sender,
                changes: subscription,
                shutdown,
            },
            worker,
        )
    }
}
impl ChainController {
    pub async fn sync_output_device(&self, output: Uuid) -> Result<(), ChainError> {
        self.request(Operation::SyncOutputDevice(output))
            .await
            .map(|_| ())
    }
    pub async fn sync_input_device(&self, input: Uuid) -> Result<(), ChainError> {
        self.request(Operation::SyncInputDevice(input))
            .await
            .map(|_| ())
    }
    /// Tenant-local coalesced invalidations, independent of the command queue.
    pub fn subscribe(&self) -> watch::Receiver<()> {
        let mut changes = self.changes.clone();
        changes.borrow_and_update();
        changes
    }
    pub fn shutdown(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Deployment coordinator only; retains durable enabled flags and bindings.
    pub async fn pause_network(&self) -> Result<(), ChainError> {
        self.request(Operation::PauseNetwork).await.map(|_| ())
    }
    pub async fn resume_network(&self) -> Result<(), ChainError> {
        self.request(Operation::ResumeNetwork).await.map(|_| ())
    }
    pub async fn configure(
        &self,
        command: crate::configuration::Command,
    ) -> Result<crate::configuration::Reply, ChainError> {
        match self.request(Operation::Configure(command)).await? {
            Reply::Configured(reply) => Ok(reply),
            _ => Err(ChainError::Unavailable),
        }
    }
    async fn request(&self, operation: Operation) -> Result<Reply, ChainError> {
        let (reply, result) = oneshot::channel();
        self.sender
            .send(Request { operation, reply })
            .await
            .map_err(|_| ChainError::Closed)?;
        result.await.map_err(|_| ChainError::Closed)?
    }
    pub async fn start(&self, input: Uuid) -> Result<(), ChainError> {
        self.request(Operation::Start(input)).await.map(|_| ())
    }
    pub async fn stop(&self, input: Uuid) -> Result<(), ChainError> {
        self.request(Operation::Stop(input)).await.map(|_| ())
    }
    pub async fn switch(&self, input: Uuid, output: Uuid) -> Result<(), ChainError> {
        self.request(Operation::Switch(input, output))
            .await
            .map(|_| ())
    }
    pub async fn unbind(&self, input: Uuid) -> Result<(), ChainError> {
        self.request(Operation::Unbind(input)).await.map(|_| ())
    }
    pub async fn status(&self) -> Result<Vec<ChainInfo>, ChainError> {
        match self.request(Operation::Status).await? {
            Reply::Chains(chains) => Ok(chains),
            _ => Err(ChainError::Unavailable),
        }
    }
    pub async fn delete(&self, device: Uuid) -> Result<DeleteOutcome, ChainError> {
        match self.request(Operation::Delete(device)).await? {
            Reply::Deleted(result) => Ok(result),
            _ => Err(ChainError::Unavailable),
        }
    }
    pub async fn deleting(&self) -> Result<Vec<Endpoint>, ChainError> {
        match self.request(Operation::Deleting).await? {
            Reply::Deleting(endpoints) => Ok(endpoints),
            _ => Err(ChainError::Unavailable),
        }
    }
}
struct Running {
    stop: CancellationToken,
    task: JoinHandle<Result<(), RuntimeError>>,
    connection: watch::Receiver<Observation>,
}
struct Actor<F> {
    store: Arc<SqliteStore>,
    factory: F,
    tenant: Uuid,
    running: HashMap<Uuid, Running>,
    uncertain: HashSet<Uuid>,
    shutdown: CancellationToken,
    changes: watch::Sender<()>,
    failures: HashMap<Uuid, Observation>,
    trackers: HashMap<Uuid, (CancellationToken, JoinHandle<()>)>,
}
impl<F: ChainFactory> Actor<F> {
    async fn cancel_tracker(&mut self, input: Uuid) -> Result<(), ChainError> {
        if let Some((stop, task)) = self.trackers.remove(&input) {
            stop.cancel();
            task.await.map_err(|_| ChainError::Worker)?;
        }
        crate::input_pairing::cancel(&self.store, self.tenant, input).await?;
        Ok(())
    }
    fn track(&mut self, input: Uuid, expires: i64) {
        let stop = CancellationToken::new();
        if let Some(task) = self.factory.input_tracker(
            self.tenant,
            input,
            expires,
            stop.clone(),
            self.changes.clone(),
        ) {
            self.trackers.insert(input, (stop, tokio::spawn(task)));
        }
    }

    async fn serve(
        &mut self,
        mut receiver: mpsc::Receiver<Request>,
        stop: &CancellationToken,
    ) -> Result<(), ChainError> {
        let attempts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT in_id,expires_at FROM input_pairings WHERE tenant_id=? AND expires_at>?",
        )
        .bind(self.tenant.to_string())
        .bind(crate::upstream::unix_time())
        .fetch_all(&self.store.pool)
        .await
        .map_err(StorageError::from)?;
        for (input, expires) in attempts {
            self.track(input.parse().map_err(|_| StorageError::Invalid)?, expires);
        }
        for input in self.store.pending_switches(self.tenant).await? {
            self.store.commit_switch(self.tenant, input).await?;
        }
        for endpoint in self.store.deleting_endpoints(self.tenant).await? {
            if stop.is_cancelled() {
                return Ok(());
            }
            if self.delete(endpoint.device_id).await.is_err() {
                tracing::warn!(event="endpoint.revoke_pending",tenant=%self.tenant,device=%endpoint.device_id);
            }
        }
        for binding in self.store.bindings(self.tenant).await? {
            if stop.is_cancelled() {
                return Ok(());
            }
            if self.store.chain_enabled(self.tenant, binding.in_id).await? {
                // A broken chain must not prevent unrelated chains from running.
                if self.start(binding.in_id).await.is_err() {
                    tracing::warn!(event="chain.restore_failed", tenant=%self.tenant, input=%binding.in_id);
                }
            }
        }
        loop {
            let request = tokio::select! { biased; _=stop.cancelled()=>return Ok(()), request=receiver.recv()=>match request { Some(request)=>request, None=>return Ok(()) } };
            let changed = !matches!(
                request.operation,
                Operation::Status
                    | Operation::SyncInputDevice(_)
                    | Operation::SyncOutputDevice(_)
                    | Operation::Deleting
                    | Operation::Configure(crate::configuration::Command::Snapshot)
            );
            let result = self.handle(request.operation).await;
            // Failed operations can also have committed a recovery checkpoint.
            if changed {
                self.changes.send_replace(());
            }
            let _ = request.reply.send(result);
        }
    }
    async fn binding(&self, input: Uuid) -> Result<Binding, ChainError> {
        self.store
            .binding(self.tenant, input)
            .await?
            .ok_or(ChainError::Unavailable)
    }
    async fn available(&self, input: Uuid) -> Result<(), ChainError> {
        if self.uncertain.contains(&input) || self.store.switch_pending(self.tenant, input).await? {
            return Err(ChainError::Unavailable);
        }
        Ok(())
    }
    async fn join(&mut self, input: Uuid) -> Result<(), ChainError> {
        if let Some(running) = self.running.remove(&input) {
            running.stop.cancel();
            if running.task.await.is_err() {
                self.uncertain.insert(input);
                return Err(ChainError::Worker);
            }
            // A runtime error has still joined its children. It does not block a cutover.
        }
        Ok(())
    }
    async fn start(&mut self, input: Uuid) -> Result<(), ChainError> {
        self.available(input).await?;
        let binding = self.binding(input).await?;
        if self
            .store
            .output_pairing_pending(self.tenant, binding.out_id)
            .await?
        {
            self.failures
                .insert(input, ConnectionStatus::InvalidState.into());
            return Err(ChainError::Unavailable);
        }
        if self
            .running
            .get(&input)
            .is_some_and(|r| !r.task.is_finished())
        {
            return Ok(());
        }
        self.join(input).await?;
        self.store
            .set_chain_enabled(self.tenant, input, true)
            .await?;
        let stop = CancellationToken::new();
        let reporter = ConnectionReporter::new(self.changes.clone());
        let connection = reporter.subscribe();
        let task = match self
            .factory
            .prepare_observed(binding, stop.clone(), reporter.clone())
            .await
        {
            Ok(task) => task,
            Err(error) => {
                self.failures
                    .insert(input, crate::runtime::status::failure(&error));
                return Err(error.into());
            }
        };
        if self.shutdown.is_cancelled() {
            return Ok(());
        }
        self.failures.remove(&input);
        self.running.insert(
            input,
            Running {
                stop,
                connection,
                task: tokio::spawn(async move {
                    let result = task.await;
                    reporter.finish(&result);
                    result
                }),
            },
        );
        Ok(())
    }
    async fn delete(&mut self, device: Uuid) -> Result<DeleteOutcome, ChainError> {
        self.cancel_tracker(device).await?;
        let endpoint = self
            .store
            .endpoint(self.tenant, device)
            .await?
            .ok_or(ChainError::Unavailable)?;
        let binding = self
            .store
            .bindings(self.tenant)
            .await?
            .into_iter()
            .find(|b| b.in_id == device || b.out_id == device);
        if let Some(binding) = &binding {
            self.available(binding.in_id).await?;
        }
        self.store.begin_delete(&endpoint).await?;
        if let Some(binding) = binding {
            self.join(binding.in_id).await?;
            self.store.unbind(self.tenant, binding.in_id).await?;
            self.failures.remove(&binding.in_id);
        }
        let result = self.factory.cleanup(endpoint).await?;
        self.failures.remove(&device);
        Ok(result)
    }
    async fn pair_output(
        &mut self,
        output: Uuid,
        code: crate::auth::SecretString,
    ) -> Result<Reply, ChainError> {
        let endpoint = self
            .store
            .endpoint(self.tenant, output)
            .await?
            .ok_or(ChainError::Unavailable)?;
        if endpoint.kind != crate::model::EndpointKind::Out
            || self.store.endpoint_deleting(self.tenant, output).await?
        {
            return Err(ChainError::Unavailable);
        }
        let binding = self
            .store
            .bindings(self.tenant)
            .await?
            .into_iter()
            .find(|b| b.out_id == output);
        if let Some(binding) = &binding {
            self.available(binding.in_id).await?;
            self.join(binding.in_id).await?;
        }
        let result = self
            .factory
            .configure(
                self.tenant,
                crate::configuration::Command::PairOut { output, code },
            )
            .await;
        // Desired enabled state stays durable. Pending pairing blocks restart after
        // errors or process exit; successful commit unlocks normal restoration.
        if let Some(binding) = binding {
            self.failures.remove(&binding.in_id);
            if self.store.chain_enabled(self.tenant, binding.in_id).await? {
                if self
                    .store
                    .output_pairing_pending(self.tenant, output)
                    .await?
                {
                    self.failures
                        .insert(binding.in_id, ConnectionStatus::InvalidState.into());
                } else if !self.shutdown.is_cancelled() {
                    self.start(binding.in_id).await?;
                }
            }
        }
        Ok(Reply::Configured(result?))
    }
    async fn recover_after(&mut self, repair: Repair) -> Result<(), ChainError> {
        for binding in self.store.bindings(self.tenant).await? {
            let input = binding.in_id;
            if !self.store.chain_enabled(self.tenant, input).await? {
                continue;
            }
            let observed = self
                .running
                .get(&input)
                .map(|r| *r.connection.borrow())
                .or_else(|| self.failures.get(&input).copied());
            let Some(observed) = observed else {
                continue;
            };
            let matches = match &repair {
                Repair::Account(account)
                    if observed.connection == ConnectionStatus::SignInRequired =>
                {
                    let device = match observed.side {
                        Some(FaultSide::Controller) => binding.in_id,
                        Some(FaultSide::Device) => binding.out_id,
                        None => continue,
                    };
                    self.store
                        .endpoint(self.tenant, device)
                        .await?
                        .is_some_and(|e| &e.account == account)
                }
                Repair::Device(output) => {
                    observed.connection == ConnectionStatus::DeviceAuthorizationRequired
                        && binding.out_id == *output
                }
                Repair::Controller(controller) => {
                    observed.connection == ConnectionStatus::ControllerRegistrationRequired
                        && input == *controller
                }
                _ => false,
            };
            if matches && !self.shutdown.is_cancelled() {
                // finish() publishes the failure just before the worker exits. Join before restarting.
                self.join(input).await?;
                if self.start(input).await.is_err() {
                    tracing::warn!(event="chain.repair_restart_failed", input=%input);
                }
            }
        }
        Ok(())
    }
    async fn handle(&mut self, operation: Operation) -> Result<Reply, ChainError> {
        match operation {
            Operation::SyncOutputDevice(output) => {
                if self.factory.sync_output_device(self.tenant, output).await? {
                    self.changes.send_replace(());
                }
                return Ok(Reply::Empty);
            }
            Operation::SyncInputDevice(input) => {
                if self
                    .trackers
                    .get(&input)
                    .is_some_and(|(_, task)| !task.is_finished())
                {
                    return Ok(Reply::Empty);
                }
                if self.factory.sync_input_device(self.tenant, input).await? {
                    self.changes.send_replace(());
                }
                return Ok(Reply::Empty);
            }
            Operation::PauseNetwork => {
                for input in self.trackers.keys().copied().collect::<Vec<_>>() {
                    self.cancel_tracker(input).await?;
                }
                for running in self.running.values() {
                    running.stop.cancel();
                }
                let mut failure = None;
                for input in self.running.keys().copied().collect::<Vec<_>>() {
                    if let Err(error) = self.join(input).await {
                        failure = Some(error);
                    }
                }
                if let Some(error) = failure {
                    return Err(error);
                }
            }
            Operation::ResumeNetwork => {
                let mut failure = None;
                for binding in self.store.bindings(self.tenant).await? {
                    if self.store.chain_enabled(self.tenant, binding.in_id).await?
                        && let Err(error) = self.start(binding.in_id).await
                    {
                        failure = Some(error);
                    }
                }
                if let Some(error) = failure {
                    return Err(error);
                }
            }
            Operation::Configure(crate::configuration::Command::PairOut { output, code }) => {
                return self.pair_output(output, code).await;
            }
            Operation::Configure(command) => {
                let pairing_input = match &command {
                    crate::configuration::Command::PairIn { input } => Some(*input),
                    _ => None,
                };
                if let Some(input) = pairing_input {
                    self.cancel_tracker(input).await?;
                }

                let repair = match &command {
                    crate::configuration::Command::PairIn { input } => {
                        Some(Repair::Controller(*input))
                    }
                    crate::configuration::Command::RecoverOut { output } => {
                        Some(Repair::Device(*output))
                    }
                    crate::configuration::Command::CheckAccount { account } => {
                        Some(Repair::Account(account.clone()))
                    }
                    _ => None,
                };
                let reply = self.factory.configure(self.tenant, command).await?;
                if let (Some(input), crate::configuration::Reply::Pairing { expires_at, .. }) =
                    (pairing_input, &reply)
                {
                    self.track(input, *expires_at);
                }
                let repair = match &reply {
                    crate::configuration::Reply::Account(account) => {
                        Some(Repair::Account(account.clone()))
                    }
                    crate::configuration::Reply::Endpoint(endpoint)
                        if endpoint.kind == crate::model::EndpointKind::Out =>
                    {
                        Some(Repair::Device(endpoint.device_id))
                    }
                    _ => repair,
                };
                if let Some(repair) = repair {
                    self.recover_after(repair).await?;
                }
                return Ok(Reply::Configured(reply));
            }
            Operation::Delete(device) => return Ok(Reply::Deleted(self.delete(device).await?)),
            Operation::Deleting => {
                return Ok(Reply::Deleting(
                    self.store.deleting_endpoints(self.tenant).await?,
                ));
            }
            Operation::Start(input) => self.start(input).await?,
            Operation::Stop(input) | Operation::Unbind(input) => {
                self.available(input).await?;
                if !self
                    .store
                    .endpoint(self.tenant, input)
                    .await?
                    .is_some_and(|endpoint| endpoint.kind == crate::model::EndpointKind::In)
                {
                    return Err(ChainError::Unavailable);
                }
                self.cancel_tracker(input).await?;
                if matches!(operation, Operation::Stop(_))
                    && self.store.binding(self.tenant, input).await?.is_none()
                {
                    return Ok(Reply::Empty);
                }
                self.binding(input).await?;
                self.store
                    .set_chain_enabled(self.tenant, input, false)
                    .await?;
                self.join(input).await?;
                self.failures.remove(&input);
                if matches!(operation, Operation::Unbind(_)) {
                    self.store.unbind(self.tenant, input).await?;
                }
            }
            Operation::Switch(input, output) => {
                self.available(input).await?;
                let previous = self.binding(input).await?;
                if previous.out_id == output {
                    return Ok(Reply::Empty);
                }
                let next = Binding {
                    out_id: output,
                    ..previous
                };
                // Preflight cannot open sockets; errors preserve the old running chain.
                drop(
                    self.factory
                        .prepare(next.clone(), CancellationToken::new())
                        .await?,
                );
                self.store.prepare_switch(&next).await?;
                self.join(input).await?;
                self.store.commit_switch(self.tenant, input).await?;
                self.failures.remove(&input);
                if self.store.chain_enabled(self.tenant, input).await? {
                    self.start(input).await?;
                }
            }
            Operation::Status => {
                let mut result = Vec::new();
                for binding in self.store.bindings(self.tenant).await? {
                    let input = binding.in_id;
                    let enabled = self.store.chain_enabled(self.tenant, input).await?;
                    let status = if self.store.switch_pending(self.tenant, input).await? {
                        ChainStatus::Switching
                    } else if self
                        .running
                        .get(&input)
                        .is_some_and(|r| !r.task.is_finished())
                    {
                        ChainStatus::Running
                    } else if enabled
                        || self.uncertain.contains(&input)
                        || self.failures.contains_key(&input)
                    {
                        ChainStatus::Failed
                    } else {
                        ChainStatus::Stopped
                    };
                    let connection = if status == ChainStatus::Stopped {
                        self.failures
                            .get(&input)
                            .copied()
                            .unwrap_or_else(|| ConnectionStatus::Stopped.into())
                    } else if let Some(running) = self.running.get(&input) {
                        let observed = *running.connection.borrow();
                        if status == ChainStatus::Failed
                            && matches!(
                                observed.connection,
                                ConnectionStatus::CheckingTarget
                                    | ConnectionStatus::TargetOffline
                                    | ConnectionStatus::Connecting
                                    | ConnectionStatus::Connected
                                    | ConnectionStatus::Reconnecting
                                    | ConnectionStatus::Stopped
                            )
                        {
                            ConnectionStatus::Failed.into()
                        } else {
                            observed
                        }
                    } else {
                        self.failures
                            .get(&input)
                            .copied()
                            .unwrap_or_else(|| ConnectionStatus::Failed.into())
                    };
                    result.push(ChainInfo {
                        binding,
                        enabled,
                        status,
                        connection: connection.connection,
                        side: connection.side,
                    });
                }
                return Ok(Reply::Chains(result));
            }
        }
        Ok(Reply::Empty)
    }
}

#[cfg(test)]
mod tests;
