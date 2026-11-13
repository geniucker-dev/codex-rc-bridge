use super::*;
use crate::{
    identity::{self, TenantSeed},
    model::{AccountIdentity, EndpointKind, Target},
};
use sqlx::sqlite::SqliteConnectOptions;
use std::sync::Mutex;
use tokio::{
    sync::Semaphore,
    time::{Duration, timeout},
};

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Configuring(Uuid, Uuid),
    Configured(Uuid),
    Cleanup(Uuid),
    Started(Uuid, Uuid),
    Stopping(Uuid),
    Joined(Uuid),
}
#[derive(Clone)]
struct Fake {
    store: Arc<SqliteStore>,
    events: mpsc::UnboundedSender<Event>,
    barriers: Arc<Mutex<HashMap<Uuid, Arc<Semaphore>>>>,
    // Number of successful preparations before a synthetic local-state failure.
    failures: Arc<Mutex<HashMap<Uuid, usize>>>,
}
impl ChainFactory for Fake {
    async fn configure(
        &self,
        tenant: Uuid,
        command: crate::configuration::Command,
    ) -> Result<crate::configuration::Reply, crate::configuration::ConfigurationError> {
        let (output, pair, fail) = match command {
            crate::configuration::Command::BeginOutLogin { output, .. } => (output, false, false),
            crate::configuration::Command::PairOut { output, code } => {
                (output, true, code.expose() == "FAIL")
            }
            _ => return Err(crate::configuration::ConfigurationError::State),
        };
        assert!(self.store.endpoint(tenant, output).await.unwrap().is_some());
        self.events
            .send(Event::Configuring(tenant, output))
            .unwrap();
        let barrier = self.barriers.lock().unwrap().get(&output).cloned();
        if let Some(barrier) = barrier {
            barrier.acquire().await.unwrap().forget();
        }
        assert!(!self.store.endpoint_deleting(tenant, output).await.unwrap());
        self.events.send(Event::Configured(output)).unwrap();
        if fail {
            sqlx::query(
                "INSERT INTO output_pairings(tenant_id,out_id) VALUES(?,?) ON CONFLICT DO NOTHING",
            )
            .bind(tenant.to_string())
            .bind(output.to_string())
            .execute(&self.store.pool)
            .await
            .unwrap();
            return Err(crate::output_pairing::PairOutputError::NoEnvironment.into());
        }
        if pair {
            sqlx::query("DELETE FROM output_pairings WHERE tenant_id=? AND out_id=?")
                .bind(tenant.to_string())
                .bind(output.to_string())
                .execute(&self.store.pool)
                .await
                .unwrap();
            return Ok(crate::configuration::Reply::PairedOutput(
                crate::output_pairing::PairedOutput {
                    environment_id: "new-target".into(),
                    name: None,
                },
            ));
        }
        Ok(crate::configuration::Reply::Done)
    }
    async fn cleanup(&self, endpoint: Endpoint) -> Result<DeleteOutcome, DeleteError> {
        assert!(
            self.store
                .bindings(endpoint.tenant_id)
                .await
                .unwrap()
                .iter()
                .all(|b| b.in_id != endpoint.device_id && b.out_id != endpoint.device_id)
        );
        self.events
            .send(Event::Cleanup(endpoint.device_id))
            .unwrap();
        Ok(DeleteOutcome::RevocationPending)
    }
    async fn prepare(
        &self,
        binding: Binding,
        stop: CancellationToken,
    ) -> Result<ChainTask, RuntimeError> {
        for (id, kind) in [
            (binding.in_id, EndpointKind::In),
            (binding.out_id, EndpointKind::Out),
        ] {
            let endpoint = self
                .store
                .endpoint(binding.tenant_id, id)
                .await
                .map_err(|_| RuntimeError::State)?
                .ok_or(RuntimeError::State)?;
            if endpoint.kind != kind
                || self
                    .store
                    .endpoint_deleting(binding.tenant_id, id)
                    .await
                    .map_err(|_| RuntimeError::State)?
            {
                return Err(RuntimeError::State);
            }
        }
        if let Some(count) = self.failures.lock().unwrap().get_mut(&binding.out_id) {
            if *count == 0 {
                return Err(RuntimeError::State);
            }
            *count -= 1;
        }
        let events = self.events.clone();
        let barriers = self.barriers.clone();
        Ok(Box::pin(async move {
            events
                .send(Event::Started(binding.in_id, binding.out_id))
                .unwrap();
            stop.cancelled().await;
            events.send(Event::Stopping(binding.in_id)).unwrap();
            let barrier = barriers.lock().unwrap().get(&binding.in_id).cloned();
            if let Some(barrier) = barrier {
                barrier.acquire().await.unwrap().forget();
            }
            events.send(Event::Joined(binding.in_id)).unwrap();
            Ok(())
        }))
    }
}
struct Fixture {
    store: Arc<SqliteStore>,
    tenant: Uuid,
    inputs: [Uuid; 3],
    outputs: [Uuid; 3],
    fake: Fake,
    events: mpsc::UnboundedReceiver<Event>,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_store(Arc::new(
            SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
                .await
                .unwrap(),
        ))
        .await
    }
    async fn with_store(store: Arc<SqliteStore>) -> Self {
        let tenant = Uuid::new_v4();
        store.create_tenant(tenant).await.unwrap();
        let account = AccountIdentity {
            workspace_id: "synthetic".into(),
            user_id: "user".into(),
        };
        store.create_account(tenant, &account).await.unwrap();
        let seed = TenantSeed::from_bytes([7; 32]);
        let mut inputs = [Uuid::nil(); 3];
        let mut outputs = [Uuid::nil(); 3];
        for (kind, ids) in [
            (EndpointKind::In, &mut inputs),
            (EndpointKind::Out, &mut outputs),
        ] {
            for (index, id) in ids.iter_mut().enumerate() {
                let endpoint = identity::endpoint(
                    &seed,
                    tenant,
                    account.clone(),
                    &format!("device-{index}"),
                    kind,
                )
                .unwrap();
                *id = endpoint.device_id;
                store.create_endpoint(&endpoint).await.unwrap();
                sqlx::query(
                    "UPDATE endpoints SET lifecycle='active' WHERE tenant_id=? AND device_id=?",
                )
                .bind(tenant.to_string())
                .bind(id.to_string())
                .execute(&store.pool)
                .await
                .unwrap();
                if kind == EndpointKind::Out {
                    store
                        .set_target(
                            tenant,
                            *id,
                            &Target {
                                environment_id: "same-real-b".into(),
                                app_server_version: "mock/1".into(),
                            },
                        )
                        .await
                        .unwrap();
                }
            }
        }
        for index in 0..2 {
            store
                .bind(&Binding {
                    tenant_id: tenant,
                    in_id: inputs[index],
                    out_id: outputs[index],
                })
                .await
                .unwrap();
        }
        let (sender, events) = mpsc::unbounded_channel();
        let fake = Fake {
            store: store.clone(),
            events: sender,
            barriers: Default::default(),
            failures: Default::default(),
        };
        Self {
            store,
            tenant,
            inputs,
            outputs,
            fake,
            events,
        }
    }
    fn spawn(
        &self,
    ) -> (
        ChainController,
        CancellationToken,
        JoinHandle<Result<(), ChainError>>,
    ) {
        let stop = CancellationToken::new();
        let (controller, task) = ChainCoordinator::spawn(
            self.store.clone(),
            self.fake.clone(),
            self.tenant,
            stop.clone(),
        );
        (controller, stop, task)
    }
    async fn event(&mut self, expected: Event) {
        assert_eq!(
            timeout(Duration::from_secs(3), self.events.recv())
                .await
                .unwrap()
                .unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn independent_chains_share_a_target_but_keep_exclusive_configuration() {
    let mut f = Fixture::new().await;
    let (control, stop, task) = f.spawn();
    for index in 0..2 {
        control.start(f.inputs[index]).await.unwrap();
        f.event(Event::Started(f.inputs[index], f.outputs[index]))
            .await;
    }
    control.start(f.inputs[0]).await.unwrap(); // Idempotent: no duplicate task.
    assert!(control.switch(f.inputs[0], f.outputs[1]).await.is_err());
    control.stop(f.inputs[0]).await.unwrap();
    f.event(Event::Stopping(f.inputs[0])).await;
    f.event(Event::Joined(f.inputs[0])).await;
    assert_eq!(
        f.store
            .binding(f.tenant, f.inputs[0])
            .await
            .unwrap()
            .unwrap()
            .out_id,
        f.outputs[0]
    );
    assert!(
        f.store
            .bind(&Binding {
                tenant_id: f.tenant,
                in_id: f.inputs[2],
                out_id: f.outputs[0]
            })
            .await
            .is_err()
    );
    let status = control.status().await.unwrap();
    assert_eq!(
        status
            .iter()
            .find(|s| s.binding.in_id == f.inputs[1])
            .unwrap()
            .status,
        ChainStatus::Running
    );
    control.unbind(f.inputs[0]).await.unwrap();
    assert!(
        f.store
            .binding(f.tenant, f.inputs[0])
            .await
            .unwrap()
            .is_none()
    );
    stop.cancel();
    task.await.unwrap().unwrap();
    f.event(Event::Stopping(f.inputs[1])).await;
    f.event(Event::Joined(f.inputs[1])).await;
}

#[tokio::test]
async fn cutover_reserves_destination_joins_old_worker_and_survives_cancelled_caller() {
    let mut f = Fixture::new().await;
    let (control, stop, task) = f.spawn();
    let input = f.inputs[0];
    control.start(input).await.unwrap();
    f.event(Event::Started(input, f.outputs[0])).await;
    assert!(
        f.store
            .claim_stream(f.tenant, input, "old-stream-hash")
            .await
            .unwrap()
    );
    let gate = Arc::new(Semaphore::new(0));
    f.fake.barriers.lock().unwrap().insert(input, gate.clone());
    let caller = tokio::spawn({
        let control = control.clone();
        let out = f.outputs[2];
        async move { control.switch(input, out).await }
    });
    f.event(Event::Stopping(input)).await;
    assert!(f.store.switch_pending(f.tenant, input).await.unwrap());
    assert_eq!(
        f.store
            .binding(f.tenant, input)
            .await
            .unwrap()
            .unwrap()
            .out_id,
        f.outputs[0]
    );
    assert!(
        f.store
            .bind(&Binding {
                tenant_id: f.tenant,
                in_id: f.inputs[2],
                out_id: f.outputs[2]
            })
            .await
            .is_err()
    );
    assert!(f.store.unbind(f.tenant, input).await.is_err());
    assert!(
        f.store
            .set_target(
                f.tenant,
                f.outputs[2],
                &Target {
                    environment_id: "changed-b".into(),
                    app_server_version: "mock/2".into()
                }
            )
            .await
            .is_err()
    );
    caller.abort();
    assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
    gate.add_permits(1);
    f.event(Event::Joined(input)).await;
    f.event(Event::Started(input, f.outputs[2])).await;
    assert_eq!(
        f.store
            .binding(f.tenant, input)
            .await
            .unwrap()
            .unwrap()
            .out_id,
        f.outputs[2]
    );
    assert!(!f.store.switch_pending(f.tenant, input).await.unwrap());
    assert!(
        !f.store
            .claim_stream(f.tenant, input, "old-stream-hash")
            .await
            .unwrap()
    );
    // Released old OUT becomes available only after the new binding commits.
    f.store
        .bind(&Binding {
            tenant_id: f.tenant,
            in_id: f.inputs[2],
            out_id: f.outputs[0],
        })
        .await
        .unwrap();
    f.fake.barriers.lock().unwrap().remove(&input);
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn preflight_failure_preserves_old_chain_but_post_commit_failure_never_falls_back() {
    let mut f = Fixture::new().await;
    let (control, stop, task) = f.spawn();
    let input = f.inputs[0];
    let output = f.outputs[2];
    control.start(input).await.unwrap();
    f.event(Event::Started(input, f.outputs[0])).await;
    f.fake.failures.lock().unwrap().insert(output, 0);
    assert!(control.switch(input, output).await.is_err());
    assert_eq!(
        f.store
            .binding(f.tenant, input)
            .await
            .unwrap()
            .unwrap()
            .out_id,
        f.outputs[0]
    );
    assert!(f.events.try_recv().is_err());
    // Preflight succeeds; reload after the durable commit fails.
    f.fake.failures.lock().unwrap().insert(output, 1);
    assert!(control.switch(input, output).await.is_err());
    f.event(Event::Stopping(input)).await;
    f.event(Event::Joined(input)).await;
    assert_eq!(
        f.store
            .binding(f.tenant, input)
            .await
            .unwrap()
            .unwrap()
            .out_id,
        output
    );
    assert_eq!(
        control
            .status()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.binding.in_id == input)
            .unwrap()
            .status,
        ChainStatus::Failed
    );
    assert!(f.events.try_recv().is_err());
    f.fake.failures.lock().unwrap().clear();
    control.start(input).await.unwrap();
    f.event(Event::Started(input, output)).await;
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn controller_scope_rejects_foreign_ids_and_disabled_switch_stays_stopped() {
    let f = Fixture::new().await;
    let other = Fixture::with_store(f.store.clone()).await;
    let (control, stop, task) = f.spawn();
    assert!(control.start(other.inputs[0]).await.is_err());
    assert!(control.stop(other.inputs[0]).await.is_err());
    assert!(control.unbind(other.inputs[0]).await.is_err());
    assert!(control.switch(f.inputs[0], other.outputs[2]).await.is_err());
    control.switch(f.inputs[0], f.outputs[2]).await.unwrap();
    let states = control.status().await.unwrap();
    assert_eq!(states.len(), 2);
    assert!(
        states
            .iter()
            .all(|s| !s.enabled && s.status == ChainStatus::Stopped)
    );
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn disk_restart_commits_reserved_switch_before_starting_enabled_chain() {
    let path = std::env::temp_dir().join(format!("bridge-chains-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let f = Fixture::with_store(Arc::new(SqliteStore::open(options.clone()).await.unwrap())).await;
    let tenant = f.tenant;
    let input = f.inputs[0];
    let output = f.outputs[2];
    f.store
        .set_chain_enabled(tenant, input, true)
        .await
        .unwrap();
    f.store
        .prepare_switch(&Binding {
            tenant_id: tenant,
            in_id: input,
            out_id: output,
        })
        .await
        .unwrap();
    // Simulate loss of the process after durable prepare and before binding commit.
    f.store.pool.close().await;
    drop(f);
    let store = Arc::new(SqliteStore::open(options).await.unwrap());
    let (events, mut receiver) = mpsc::unbounded_channel();
    let fake = Fake {
        store: store.clone(),
        events,
        barriers: Default::default(),
        failures: Default::default(),
    };
    let stop = CancellationToken::new();
    let (control, task) = ChainCoordinator::spawn(store.clone(), fake, tenant, stop.clone());
    assert_eq!(
        timeout(Duration::from_secs(3), receiver.recv())
            .await
            .unwrap()
            .unwrap(),
        Event::Started(input, output)
    );
    assert!(!store.switch_pending(tenant, input).await.unwrap());
    let states = control.status().await.unwrap();
    assert_eq!(
        states
            .iter()
            .filter(|s| s.status == ChainStatus::Running)
            .count(),
        1
    );
    stop.cancel();
    task.await.unwrap().unwrap();
    store.pool.close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn reopen_preserves_existing_binding_and_does_not_enable_it_implicitly() {
    let path = std::env::temp_dir().join(format!("bridge-reopen-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
    let mut f = Fixture::with_store(store).await;
    let saved = f.store.bindings(f.tenant).await.unwrap();
    f.store.pool.close().await;
    let store = Arc::new(SqliteStore::open(options).await.unwrap());
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(version, 14);
    assert_eq!(store.bindings(f.tenant).await.unwrap(), saved);
    f.store = store.clone();
    f.fake.store = store.clone();
    let (control, stop, task) = f.spawn();
    assert!(control.status().await.unwrap().iter().all(|s| !s.enabled));
    stop.cancel();
    task.await.unwrap().unwrap();
    store.pool.close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn concurrent_switches_cannot_share_one_destination() {
    let f = Fixture::new().await;
    let (control, stop, task) = f.spawn();
    let (first, second) = tokio::join!(
        control.switch(f.inputs[0], f.outputs[2]),
        control.switch(f.inputs[1], f.outputs[2]),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    assert_eq!(
        f.store
            .bindings(f.tenant)
            .await
            .unwrap()
            .iter()
            .filter(|b| b.out_id == f.outputs[2])
            .count(),
        1
    );
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn shutdown_during_cutover_finishes_commit_without_opening_new_chain() {
    let mut f = Fixture::new().await;
    let (control, stop, task) = f.spawn();
    let input = f.inputs[0];
    let output = f.outputs[2];
    control.start(input).await.unwrap();
    f.event(Event::Started(input, f.outputs[0])).await;
    let gate = Arc::new(Semaphore::new(0));
    f.fake.barriers.lock().unwrap().insert(input, gate.clone());
    let request = tokio::spawn(async move { control.switch(input, output).await });
    f.event(Event::Stopping(input)).await;
    stop.cancel();
    gate.add_permits(1);
    request.await.unwrap().unwrap();
    task.await.unwrap().unwrap();
    f.event(Event::Joined(input)).await;
    assert!(f.events.try_recv().is_err());
    assert_eq!(
        f.store
            .binding(f.tenant, input)
            .await
            .unwrap()
            .unwrap()
            .out_id,
        output
    );
    assert!(f.store.chain_enabled(f.tenant, input).await.unwrap());
    assert!(!f.store.switch_pending(f.tenant, input).await.unwrap());
}

#[tokio::test]
async fn deletion_waits_for_old_worker_before_unbinding_or_revoking_and_does_not_stop_other_chain()
{
    let mut f = Fixture::new().await;
    let (control, stop, task) = f.spawn();
    for i in 0..2 {
        control.start(f.inputs[i]).await.unwrap();
        f.event(Event::Started(f.inputs[i], f.outputs[i])).await;
    }
    let gate = Arc::new(Semaphore::new(0));
    f.fake
        .barriers
        .lock()
        .unwrap()
        .insert(f.inputs[0], gate.clone());
    let output = f.outputs[0];
    let request = tokio::spawn({
        let control = control.clone();
        async move { control.delete(output).await }
    });
    f.event(Event::Stopping(f.inputs[0])).await;
    assert!(f.store.endpoint_deleting(f.tenant, output).await.unwrap());
    assert!(
        f.store
            .binding(f.tenant, f.inputs[0])
            .await
            .unwrap()
            .is_some()
    );
    assert!(f.events.try_recv().is_err());
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    gate.add_permits(1);
    f.event(Event::Joined(f.inputs[0])).await;
    f.event(Event::Cleanup(output)).await;
    assert!(
        f.store
            .binding(f.tenant, f.inputs[0])
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(control.deleting().await.unwrap().len(), 1);
    assert_eq!(
        control.status().await.unwrap()[0].status,
        ChainStatus::Running
    );
    assert!(control.start(f.inputs[0]).await.is_err());
    stop.cancel();
    task.await.unwrap().unwrap();
    f.event(Event::Stopping(f.inputs[1])).await;
    f.event(Event::Joined(f.inputs[1])).await;
    // Restart resumes pending revoke before starting any surviving enabled chain.
    let (_control, stop, task) = f.spawn();
    f.event(Event::Cleanup(output)).await;
    f.event(Event::Started(f.inputs[1], f.outputs[1])).await;
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn cancelled_configuration_finishes_before_queued_deletion() {
    let mut f = Fixture::new().await;
    let output = f.outputs[2];
    let gate = Arc::new(Semaphore::new(0));
    f.fake.barriers.lock().unwrap().insert(output, gate.clone());
    let (control, stop, worker) = f.spawn();
    let caller = tokio::spawn({
        let control = control.clone();
        async move {
            control
                .configure(crate::configuration::Command::BeginOutLogin { port: 1455, output })
                .await
        }
    });
    f.event(Event::Configuring(f.tenant, output)).await;
    caller.abort();
    assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
    let mut deletion = tokio::spawn(async move { control.delete(output).await });
    assert!(
        timeout(Duration::from_millis(30), &mut deletion)
            .await
            .is_err()
    );
    assert!(!f.store.endpoint_deleting(f.tenant, output).await.unwrap());
    gate.add_permits(1);
    f.event(Event::Configured(output)).await;
    f.event(Event::Cleanup(output)).await;
    assert_eq!(
        deletion.await.unwrap().unwrap(),
        DeleteOutcome::RevocationPending
    );
    stop.cancel();
    worker.await.unwrap().unwrap();
}

#[tokio::test]
async fn deployment_pause_drains_runtime_without_disabling_or_releasing_bindings() {
    let mut f = Fixture::new().await;
    let (controller, stop, task) = f.spawn();
    let first = f.inputs[0];
    controller.start(first).await.unwrap();
    f.event(Event::Started(first, f.outputs[0])).await;
    controller.pause_network().await.unwrap();
    f.event(Event::Stopping(first)).await;
    f.event(Event::Joined(first)).await;
    assert!(f.store.chain_enabled(f.tenant, first).await.unwrap());
    assert!(!f.store.chain_enabled(f.tenant, f.inputs[1]).await.unwrap());
    assert_eq!(
        f.store
            .binding(f.tenant, first)
            .await
            .unwrap()
            .unwrap()
            .out_id,
        f.outputs[0]
    );
    controller.resume_network().await.unwrap();
    f.event(Event::Started(first, f.outputs[0])).await;
    assert!(!f.store.chain_enabled(f.tenant, f.inputs[1]).await.unwrap());
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn repairing_output_joins_old_workers_and_restores_only_the_original_run_intent() {
    let mut f = Fixture::new().await;
    let (control, stop, task) = f.spawn();
    control.start(f.inputs[0]).await.unwrap();
    f.event(Event::Started(f.inputs[0], f.outputs[0])).await;
    control.start(f.inputs[1]).await.unwrap();
    f.event(Event::Started(f.inputs[1], f.outputs[1])).await;
    let barrier = Arc::new(Semaphore::new(0));
    f.fake
        .barriers
        .lock()
        .unwrap()
        .insert(f.inputs[0], barrier.clone());
    let pairing = tokio::spawn({
        let control = control.clone();
        let output = f.outputs[0];
        async move {
            control
                .configure(crate::configuration::Command::PairOut {
                    output,
                    code: crate::auth::SecretString::new("ABCD-EFGH".into()),
                })
                .await
        }
    });
    f.event(Event::Stopping(f.inputs[0])).await;
    assert!(
        timeout(Duration::from_millis(30), f.events.recv())
            .await
            .is_err()
    );
    // A conflicting operation queues behind the join and pairing, instead of mutating early.
    let stopping = tokio::spawn({
        let control = control.clone();
        let input = f.inputs[0];
        async move { control.stop(input).await }
    });
    assert!(!stopping.is_finished());
    barrier.add_permits(2);
    pairing.await.unwrap().unwrap();
    f.event(Event::Joined(f.inputs[0])).await;
    f.event(Event::Configuring(f.tenant, f.outputs[0])).await;
    f.event(Event::Configured(f.outputs[0])).await;
    f.event(Event::Started(f.inputs[0], f.outputs[0])).await;
    stopping.await.unwrap().unwrap();
    f.event(Event::Stopping(f.inputs[0])).await;
    f.event(Event::Joined(f.inputs[0])).await;
    // A paused binding stays paused; the other connection keeps running throughout.
    control
        .configure(crate::configuration::Command::PairOut {
            output: f.outputs[0],
            code: crate::auth::SecretString::new("ABCD-EFGH".into()),
        })
        .await
        .unwrap();
    f.event(Event::Configuring(f.tenant, f.outputs[0])).await;
    f.event(Event::Configured(f.outputs[0])).await;
    let status = control.status().await.unwrap();
    assert!(
        !status
            .iter()
            .find(|s| s.binding.in_id == f.inputs[0])
            .unwrap()
            .enabled
    );
    assert_eq!(
        status
            .iter()
            .find(|s| s.binding.in_id == f.inputs[1])
            .unwrap()
            .status,
        ChainStatus::Running
    );
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn incomplete_repair_keeps_binding_and_run_intent_but_waits_for_successful_retry() {
    let mut f = Fixture::new().await;
    let (control, stop, task) = f.spawn();
    control.start(f.inputs[0]).await.unwrap();
    f.event(Event::Started(f.inputs[0], f.outputs[0])).await;
    assert!(
        control
            .configure(crate::configuration::Command::PairOut {
                output: f.outputs[0],
                code: crate::auth::SecretString::new("FAIL".into())
            })
            .await
            .is_err()
    );
    f.event(Event::Stopping(f.inputs[0])).await;
    f.event(Event::Joined(f.inputs[0])).await;
    f.event(Event::Configuring(f.tenant, f.outputs[0])).await;
    f.event(Event::Configured(f.outputs[0])).await;
    let status = control.status().await.unwrap();
    let chain = status
        .iter()
        .find(|s| s.binding.in_id == f.inputs[0])
        .unwrap();
    assert!(chain.enabled);
    assert_eq!(chain.status, ChainStatus::Failed);
    assert_eq!(chain.connection, ConnectionStatus::InvalidState);
    control
        .configure(crate::configuration::Command::PairOut {
            output: f.outputs[0],
            code: crate::auth::SecretString::new("ABCD-EFGH".into()),
        })
        .await
        .unwrap();
    f.event(Event::Configuring(f.tenant, f.outputs[0])).await;
    f.event(Event::Configured(f.outputs[0])).await;
    f.event(Event::Started(f.inputs[0], f.outputs[0])).await;
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[derive(Clone, Copy)]
enum Fault {
    ControllerAccount,
    DeviceAccount,
    DeviceAuthorization,
    ControllerRegistration,
}
impl Fault {
    fn error(self) -> RuntimeError {
        use crate::{auth::AuthError, upstream::ControlError};
        let (error, side) = match self {
            Self::ControllerAccount => (
                ControlError::Auth(AuthError::SignInRequired),
                FaultSide::Controller,
            ),
            Self::DeviceAccount => (
                ControlError::Auth(AuthError::SignInRequired),
                FaultSide::Device,
            ),
            Self::DeviceAuthorization => (ControlError::Registration, FaultSide::Device),
            Self::ControllerRegistration => {
                (ControlError::HostRegistrationMissing, FaultSide::Controller)
            }
        };
        RuntimeError::from(error).at(side)
    }
}
#[derive(Clone)]
struct HealingFactory {
    fake: Fake,
    faults: Arc<Mutex<HashMap<Uuid, Fault>>>,
    input: Uuid,
    output: Uuid,
}
impl ChainFactory for HealingFactory {
    async fn prepare(
        &self,
        binding: Binding,
        stop: CancellationToken,
    ) -> Result<ChainTask, RuntimeError> {
        let fault = self.faults.lock().unwrap().get(&binding.in_id).copied();
        if let Some(fault) = fault {
            return Err(fault.error());
        }
        self.fake.prepare(binding, stop).await
    }
    async fn configure(
        &self,
        tenant: Uuid,
        command: crate::configuration::Command,
    ) -> Result<crate::configuration::Reply, crate::configuration::ConfigurationError> {
        use crate::configuration::{Command, Reply};
        let endpoint = self
            .fake
            .store
            .endpoint(tenant, self.output)
            .await?
            .unwrap();
        match command {
            Command::CompleteLogin { callback, .. } if callback.expose() == "unrelated-account" => {
                Ok(Reply::Account(AccountIdentity {
                    workspace_id: "unrelated".into(),
                    user_id: "other".into(),
                }))
            }
            Command::CompleteLogin { callback, .. } => {
                self.faults.lock().unwrap().remove(&self.input);
                if callback.expose() == "device" {
                    Ok(Reply::Endpoint(endpoint))
                } else {
                    Ok(Reply::Account(endpoint.account))
                }
            }
            Command::PairIn { input } => {
                self.faults.lock().unwrap().remove(&input);
                Ok(Reply::Pairing {
                    code: crate::auth::SecretString::new("SYNTHETIC".into()),
                    expires_at: 0,
                })
            }
            _ => Err(crate::configuration::ConfigurationError::State),
        }
    }
}
#[tokio::test]
async fn each_repair_restarts_only_matching_enabled_failures() {
    use crate::{auth::SecretString, configuration::Command};
    for fault in [
        Fault::ControllerAccount,
        Fault::DeviceAccount,
        Fault::DeviceAuthorization,
        Fault::ControllerRegistration,
    ] {
        let mut f = Fixture::new().await;
        f.store
            .bind(&Binding {
                tenant_id: f.tenant,
                in_id: f.inputs[2],
                out_id: f.outputs[2],
            })
            .await
            .unwrap();
        f.store
            .set_chain_enabled(f.tenant, f.inputs[0], true)
            .await
            .unwrap();
        let faults = Arc::new(Mutex::new(HashMap::from([
            (f.inputs[0], fault),
            (f.inputs[2], fault),
        ])));
        let factory = HealingFactory {
            fake: f.fake.clone(),
            faults,
            input: f.inputs[0],
            output: f.outputs[0],
        };
        let stop = CancellationToken::new();
        let (controller, task) =
            ChainCoordinator::spawn(f.store.clone(), factory, f.tenant, stop.clone());
        controller.start(f.inputs[1]).await.unwrap();
        let statuses = controller.status().await.unwrap();
        let first = statuses
            .iter()
            .find(|c| c.binding.in_id == f.inputs[0])
            .unwrap();
        let expected = crate::runtime::status::failure(&fault.error());
        assert_eq!(
            (first.connection, first.side),
            (expected.connection, expected.side)
        );
        controller
            .configure(Command::CompleteLogin {
                transaction: Uuid::new_v4(),
                callback: SecretString::new("unrelated-account".into()),
            })
            .await
            .unwrap();
        assert_eq!(
            controller
                .status()
                .await
                .unwrap()
                .iter()
                .find(|c| c.binding.in_id == f.inputs[0])
                .unwrap()
                .status,
            ChainStatus::Failed
        );
        let command = match fault {
            Fault::ControllerRegistration => Command::PairIn { input: f.inputs[0] },
            _ => Command::CompleteLogin {
                transaction: Uuid::new_v4(),
                callback: SecretString::new(
                    if matches!(fault, Fault::DeviceAuthorization) {
                        "device"
                    } else {
                        "account"
                    }
                    .into(),
                ),
            },
        };
        controller.configure(command).await.unwrap();
        for chain in controller.status().await.unwrap() {
            assert_eq!(chain.side, None);
            assert_eq!(
                chain.status,
                if chain.binding.in_id == f.inputs[2] {
                    ChainStatus::Stopped
                } else {
                    ChainStatus::Running
                }
            );
        }
        stop.cancel();
        task.await.unwrap().unwrap();
        let mut starts = HashMap::new();
        while let Ok(event) = f.events.try_recv() {
            if let Event::Started(input, _) = event {
                *starts.entry(input).or_insert(0) += 1;
            }
        }
        assert_eq!(starts.get(&f.inputs[0]), Some(&1));
        assert_eq!(starts.get(&f.inputs[1]), Some(&1));
        assert!(!starts.contains_key(&f.inputs[2]));
    }
}
