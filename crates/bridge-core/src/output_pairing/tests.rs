use super::*;
use crate::{
    auth::AuthService,
    identity,
    model::{AccountIdentity, Binding},
    vault::{EncryptionKey, SecretScope},
};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use sqlx::sqlite::SqliteConnectOptions;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Default)]
struct Mock {
    grants: Mutex<HashMap<String, Vec<String>>>,
    calls: Mutex<Vec<(Method, String)>>,
    result_count: AtomicUsize,
    fail_revoke: AtomicBool,
    absent_revoke: AtomicBool,
    fail_pair: AtomicBool,
    missing_name: AtomicBool,
    environments: Mutex<HashMap<String, Value>>,
    fail_list: AtomicBool,
    reject_account: AtomicBool,
}
async fn handler(State(mock): State<Arc<Mock>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    mock.calls
        .lock()
        .unwrap()
        .push((method.clone(), path.clone()));
    if path == "/oauth/token" {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":"invalid_grant"})),
        )
            .into_response();
    }
    if let Some(client) = path
        .strip_prefix("/backend-api/codex/remote/control/clients/")
        .and_then(|p| p.strip_suffix("/environments"))
    {
        if mock.fail_list.load(Ordering::SeqCst) {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if mock.reject_account.load(Ordering::SeqCst) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let list = mock
            .grants
            .lock()
            .unwrap()
            .get(client)
            .cloned()
            .unwrap_or_default();
        return axum::Json(json!({"items":list.into_iter().map(|env| mock.environments.lock().unwrap().get(&env).cloned().unwrap_or_else(|| json!({"env_id":env,"display_name":if mock.missing_name.load(Ordering::SeqCst) {None} else {Some("Studio")},"app_server_version":"codex-test/1","online":true}))).collect::<Vec<_>>(),"cursor":null})).into_response();
    }
    if method == Method::DELETE {
        let parts: Vec<_> = path.split('/').collect();
        assert_eq!(parts[5], "environments");
        assert_eq!(parts[7], "clients");
        assert_eq!(parts[8], "own-out");
        if mock.fail_revoke.load(Ordering::SeqCst) {
            return if mock.absent_revoke.load(Ordering::SeqCst) {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            }
            .into_response();
        }
        mock.grants
            .lock()
            .unwrap()
            .get_mut("own-out")
            .unwrap()
            .retain(|e| e != parts[6]);
        return if mock.absent_revoke.load(Ordering::SeqCst) {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::NO_CONTENT
        }
        .into_response();
    }
    if path.ends_with("/client/pair") {
        assert!(
            mock.grants
                .lock()
                .unwrap()
                .get("own-out")
                .unwrap()
                .is_empty()
        );
        let payload: Value =
            serde_json::from_slice(&to_bytes(request.into_body(), 8192).await.unwrap()).unwrap();
        assert_eq!(payload["client_id"], "own-out");
        if mock.fail_pair.load(Ordering::SeqCst) {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let n = mock.result_count.load(Ordering::SeqCst);
        mock.grants.lock().unwrap().insert(
            "own-out".into(),
            (0..n).map(|i| format!("new-{i}")).collect(),
        );
        return axum::Json(json!({})).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
struct Fixture {
    store: Arc<SqliteStore>,
    vault: Vault,
    auth: AuthService<Upstream>,
    upstream: Upstream,
    tenant: Uuid,
    output: Uuid,
    input: Uuid,
    mock: Arc<Mock>,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_options(SqliteConnectOptions::new().in_memory(true)).await
    }
    async fn with_options(options: SqliteConnectOptions) -> Self {
        let store = Arc::new(SqliteStore::open(options).await.unwrap());
        let tenant = Uuid::new_v4();
        store.create_tenant(tenant).await.unwrap();
        let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[21; 32]).unwrap());
        let account = AccountIdentity {
            workspace_id: "synthetic-workspace".into(),
            user_id: "synthetic-user".into(),
        };
        store.create_account(tenant, &account).await.unwrap();
        vault.write(tenant,&SecretScope::Account(account.clone()),&serde_json::to_vec(&json!({"account":account,"access":"synthetic-access","refresh":"synthetic-refresh","expires_at":crate::upstream::unix_time()+3600})).unwrap()).await.unwrap();
        let seed = vault.tenant_seed(tenant).await.unwrap();
        let output =
            identity::endpoint(&seed, tenant, account.clone(), "desk", EndpointKind::Out).unwrap();
        let input =
            identity::endpoint(&seed, tenant, account.clone(), "phone", EndpointKind::In).unwrap();
        store.create_endpoint(&output).await.unwrap();
        store.create_endpoint(&input).await.unwrap();
        vault
            .save_out_registration(
                tenant,
                output.device_id,
                &Registration {
                    client_id: "own-out".into(),
                    account_user_id: account.user_id,
                    key_id: crate::device::identity(
                        &identity::signing_key(
                            &seed,
                            tenant,
                            &output.account,
                            &output.identity_name,
                        )
                        .unwrap(),
                    )
                    .key_id,
                },
            )
            .await
            .unwrap();
        vault
            .save_host_registration(
                tenant,
                input.device_id,
                &crate::upstream::HostRegistration {
                    server_id: "host".into(),
                    environment_id: "host-env".into(),
                },
            )
            .await
            .unwrap();
        store
            .set_target(
                tenant,
                output.device_id,
                &Target {
                    environment_id: "old-0".into(),
                    app_server_version: "old/1".into(),
                },
            )
            .await
            .unwrap();
        store
            .observe_target(
                tenant,
                output.device_id,
                "old-0",
                &crate::output_sync::TargetObservation {
                    record: Some(crate::output_sync::TargetRecord {
                        name: Some("Old name".into()),
                        ..Default::default()
                    }),
                    error: None,
                },
            )
            .await
            .unwrap();
        store
            .bind(&Binding {
                tenant_id: tenant,
                in_id: input.device_id,
                out_id: output.device_id,
            })
            .await
            .unwrap();
        let mock = Arc::new(Mock::default());
        mock.result_count.store(1, Ordering::SeqCst);
        mock.grants
            .lock()
            .unwrap()
            .insert("own-out".into(), vec!["old-0".into(), "old-1".into()]);
        mock.grants
            .lock()
            .unwrap()
            .insert("other-out".into(), vec!["old-0".into()]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = Upstream::local_test(&format!("http://{}", listener.local_addr().unwrap()));
        let auth = AuthService::new(vault.clone(), upstream.clone());
        let app = axum::Router::new()
            .fallback(any(handler))
            .with_state(mock.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            store,
            vault,
            auth,
            upstream,
            tenant,
            output: output.device_id,
            input: input.device_id,
            mock,
            task,
        }
    }
    async fn pair(&self) -> Result<PairedOutput, PairOutputError> {
        pair(
            &self.vault,
            &self.auth,
            &self.upstream,
            self.tenant,
            self.output,
            &SecretString::new("ABCD-EFGH".into()),
        )
        .await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn clears_all_own_grants_and_commits_unique_target_and_name_with_binding_intact() {
    let f = Fixture::new().await;
    let paired = f.pair().await.unwrap();
    assert_eq!(
        serde_json::to_value(paired).unwrap(),
        json!({"environment_id":"new-0","name":"Studio"})
    );
    assert_eq!(
        f.store
            .target(f.tenant, f.output)
            .await
            .unwrap()
            .unwrap()
            .environment_id,
        "new-0"
    );
    assert_eq!(
        f.store
            .target_record(f.tenant, f.output)
            .await
            .unwrap()
            .name
            .as_deref(),
        Some("Studio")
    );
    assert_eq!(
        f.store
            .binding(f.tenant, f.input)
            .await
            .unwrap()
            .unwrap()
            .out_id,
        f.output
    );
    assert!(
        !f.store
            .output_pairing_pending(f.tenant, f.output)
            .await
            .unwrap()
    );
    let grants = f.mock.grants.lock().unwrap();
    assert_eq!(grants["own-out"], vec!["new-0"]);
    assert_eq!(grants["other-out"], vec!["old-0"]);
    let calls = f.mock.calls.lock().unwrap();
    let paired = calls
        .iter()
        .position(|(_, p)| p.ends_with("/client/pair"))
        .unwrap();
    assert_eq!(
        calls[..paired]
            .iter()
            .filter(|(m, _)| m == Method::DELETE)
            .count(),
        2
    );
}
#[tokio::test]
async fn cleanup_failure_preserves_target_binding_and_can_retry() {
    let f = Fixture::new().await;
    f.mock.fail_revoke.store(true, Ordering::SeqCst);
    assert!(matches!(
        f.pair().await,
        Err(PairOutputError::CleanupPending)
    ));
    assert_eq!(
        f.store
            .target(f.tenant, f.output)
            .await
            .unwrap()
            .unwrap()
            .environment_id,
        "old-0"
    );
    assert_eq!(
        f.store
            .target_record(f.tenant, f.output)
            .await
            .unwrap()
            .name
            .as_deref(),
        Some("Old name")
    );
    assert!(f.store.binding(f.tenant, f.input).await.unwrap().is_some());
    assert!(
        f.store
            .output_pairing_pending(f.tenant, f.output)
            .await
            .unwrap()
    );
    assert!(
        f.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, p)| !p.ends_with("/client/pair"))
    );
    f.mock.fail_revoke.store(false, Ordering::SeqCst);
    f.pair().await.unwrap();
}
#[tokio::test]
async fn missing_environment_revoke_404_is_confirmed_by_controller_access_list() {
    let f = Fixture::new().await;
    f.mock.absent_revoke.store(true, Ordering::SeqCst);
    f.mock.missing_name.store(true, Ordering::SeqCst);
    assert!(f.pair().await.unwrap().name.is_none());
    assert!(
        f.store
            .target_record(f.tenant, f.output)
            .await
            .unwrap()
            .name
            .is_none()
    );
}
#[tokio::test]
async fn zero_multiple_and_failed_pair_never_replace_the_saved_target() {
    let f = Fixture::new().await;
    for count in [0, 2] {
        f.mock.result_count.store(count, Ordering::SeqCst);
        let err = f.pair().await.unwrap_err();
        assert!(matches!(
            (&err, count),
            (PairOutputError::NoEnvironment, 0) | (PairOutputError::MultipleEnvironments, 2)
        ));
        assert_eq!(
            f.store
                .target(f.tenant, f.output)
                .await
                .unwrap()
                .unwrap()
                .environment_id,
            "old-0"
        );
        assert!(
            f.store
                .output_pairing_pending(f.tenant, f.output)
                .await
                .unwrap()
        );
    }
    f.mock.fail_pair.store(true, Ordering::SeqCst);
    assert!(matches!(f.pair().await, Err(PairOutputError::Control(_))));
    f.mock.fail_pair.store(false, Ordering::SeqCst);
    f.mock.result_count.store(1, Ordering::SeqCst);
    f.pair().await.unwrap();
}
#[tokio::test]
async fn invalid_code_foreign_tenant_wrong_role_and_deleting_output_have_no_effects() {
    let f = Fixture::new().await;
    assert!(
        pair(
            &f.vault,
            &f.auth,
            &f.upstream,
            f.tenant,
            f.output,
            &SecretString::new("bad".into())
        )
        .await
        .is_err()
    );
    for (tenant, output) in [(Uuid::new_v4(), f.output), (f.tenant, f.input)] {
        assert!(
            pair(
                &f.vault,
                &f.auth,
                &f.upstream,
                tenant,
                output,
                &SecretString::new("ABCD-EFGH".into())
            )
            .await
            .is_err()
        );
    }
    sqlx::query("UPDATE endpoints SET lifecycle='deleting' WHERE tenant_id=? AND device_id=?")
        .bind(f.tenant.to_string())
        .bind(f.output.to_string())
        .execute(&f.store.pool)
        .await
        .unwrap();
    assert!(f.pair().await.is_err());
    assert!(f.mock.calls.lock().unwrap().is_empty());
    assert!(
        !f.store
            .output_pairing_pending(f.tenant, f.output)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn failed_target_commit_keeps_old_metadata_and_retries_from_remote_grants() {
    let f = Fixture::new().await;
    sqlx::raw_sql("CREATE TRIGGER fail_target_commit BEFORE UPDATE ON targets BEGIN SELECT RAISE(ABORT,'synthetic storage failure'); END;")
        .execute(&f.store.pool).await.unwrap();
    assert!(matches!(f.pair().await, Err(PairOutputError::Storage(_))));
    assert!(
        f.store
            .output_pairing_pending(f.tenant, f.output)
            .await
            .unwrap()
    );
    assert_eq!(
        f.store
            .target(f.tenant, f.output)
            .await
            .unwrap()
            .unwrap()
            .environment_id,
        "old-0"
    );
    assert_eq!(
        f.store
            .target_record(f.tenant, f.output)
            .await
            .unwrap()
            .name
            .as_deref(),
        Some("Old name")
    );
    sqlx::raw_sql("DROP TRIGGER fail_target_commit;")
        .execute(&f.store.pool)
        .await
        .unwrap();
    f.pair().await.unwrap();
    assert!(
        !f.store
            .output_pairing_pending(f.tenant, f.output)
            .await
            .unwrap()
    );
}

#[test]
fn public_errors_are_distinct_and_shared_by_desktop_and_http() {
    for (error, code) in [
        (
            PairOutputError::CleanupPending,
            "out_pairing_cleanup_pending",
        ),
        (PairOutputError::NoEnvironment, "out_pairing_no_environment"),
        (
            PairOutputError::MultipleEnvironments,
            "out_pairing_multiple_environments",
        ),
    ] {
        let mapped = crate::operation_error::OperationError::from(
            crate::chains::ChainError::Configuration(error.into()),
        );
        assert_eq!(mapped.status, 409);
        assert_eq!(mapped.code, code);
    }
}

#[tokio::test]
async fn interrupted_pair_survives_database_reopen_and_blocks_runtime_until_retry() {
    let path = std::env::temp_dir().join(format!("bridge-pair-reopen-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let f = Fixture::with_options(options.clone()).await;
    f.mock.fail_pair.store(true, Ordering::SeqCst);
    assert!(f.pair().await.is_err());
    f.store
        .set_chain_enabled(f.tenant, f.input, true)
        .await
        .unwrap();
    assert!(
        f.store
            .claim_stream(f.tenant, f.input, "retired-synthetic-stream")
            .await
            .unwrap()
    );
    f.store.pool.close().await;
    let store = Arc::new(SqliteStore::open(options).await.unwrap());
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[21; 32]).unwrap());
    let auth = Arc::new(AuthService::new(vault.clone(), f.upstream.clone()));
    let factory = crate::chains::RuntimeFactory {
        store: store.clone(),
        vault: vault.clone(),
        auth: auth.clone(),
        upstream: f.upstream.clone(),
    };
    let assert_snapshot = async |pending: bool| {
        let snapshot = serde_json::to_value(
            factory
                .configure(f.tenant, crate::configuration::Command::Snapshot)
                .await
                .unwrap(),
        )
        .unwrap();
        let endpoints = snapshot["value"]["endpoints"].as_array().unwrap();
        assert_eq!(endpoints.len(), 2);
        for endpoint in endpoints {
            let output = endpoint["endpoint"]["device_id"] == f.output.to_string();
            assert_eq!(endpoint["pairing_pending"], output && pending);
            if output {
                assert_eq!(
                    endpoint["target"]["environment_id"],
                    if pending { "old-0" } else { "new-0" }
                );
            }
        }
    };
    assert_snapshot(true).await;
    assert!(
        store
            .output_pairing_pending(f.tenant, f.output)
            .await
            .unwrap()
    );
    assert!(
        crate::runtime::BridgeRuntime::load(
            store.clone(),
            vault.clone(),
            auth.clone(),
            f.upstream.clone(),
            f.tenant,
            f.input
        )
        .await
        .is_err()
    );
    assert!(store.chain_enabled(f.tenant, f.input).await.unwrap());
    assert!(
        !store
            .claim_stream(f.tenant, f.input, "retired-synthetic-stream")
            .await
            .unwrap()
    );
    f.mock.fail_pair.store(false, Ordering::SeqCst);
    pair(
        &vault,
        &auth,
        &f.upstream,
        f.tenant,
        f.output,
        &SecretString::new("ABCD-EFGH".into()),
    )
    .await
    .unwrap();
    assert_snapshot(false).await;
    assert!(
        crate::runtime::BridgeRuntime::load(
            store.clone(),
            vault,
            auth,
            f.upstream.clone(),
            f.tenant,
            f.input
        )
        .await
        .is_ok()
    );
    store.pool.close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn clears_more_than_one_cleanup_batch_before_pairing() {
    let f = Fixture::new().await;
    f.mock.grants.lock().unwrap().insert(
        "own-out".into(),
        (0..35).map(|i| format!("old-{i}")).collect(),
    );
    f.pair().await.unwrap();
    assert_eq!(
        f.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == Method::DELETE)
            .count(),
        35
    );
}

#[tokio::test]
async fn revoke_404_with_access_still_present_is_not_success() {
    let f = Fixture::new().await;
    f.mock.fail_revoke.store(true, Ordering::SeqCst);
    f.mock.absent_revoke.store(true, Ordering::SeqCst);
    assert!(matches!(
        f.pair().await,
        Err(PairOutputError::CleanupPending)
    ));
    assert!(
        f.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, p)| !p.ends_with("/client/pair"))
    );
}

#[tokio::test]
async fn first_pair_creates_a_target_and_keeps_the_registered_identity() {
    let f = Fixture::new().await;
    f.store.unbind(f.tenant, f.input).await.unwrap();
    sqlx::query("DELETE FROM targets WHERE tenant_id=? AND out_id=?")
        .bind(f.tenant.to_string())
        .bind(f.output.to_string())
        .execute(&f.store.pool)
        .await
        .unwrap();
    f.mock
        .grants
        .lock()
        .unwrap()
        .insert("own-out".into(), vec![]);
    let before = f
        .vault
        .read(f.tenant, &SecretScope::OutRegistration(f.output))
        .await
        .unwrap()
        .unwrap();
    f.pair().await.unwrap();
    let after = f
        .vault
        .read(f.tenant, &SecretScope::OutRegistration(f.output))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&*before, &*after);
    assert_eq!(
        f.store
            .target(f.tenant, f.output)
            .await
            .unwrap()
            .unwrap()
            .environment_id,
        "new-0"
    );
}

impl Fixture {
    fn factory(&self) -> crate::chains::RuntimeFactory {
        crate::chains::RuntimeFactory {
            store: self.store.clone(),
            vault: self.vault.clone(),
            auth: Arc::new(AuthService::new(self.vault.clone(), self.upstream.clone())),
            upstream: self.upstream.clone(),
        }
    }
    async fn sync_at(&self, now: i64) -> bool {
        let endpoint = self
            .store
            .endpoint(self.tenant, self.output)
            .await
            .unwrap()
            .unwrap();
        crate::output_sync::sync(&self.factory(), &endpoint, now)
            .await
            .unwrap()
    }
    async fn record(&self) -> crate::output_sync::TargetRecord {
        self.store
            .target_record(self.tenant, self.output)
            .await
            .unwrap()
    }
    fn only_target(&self) {
        self.mock
            .grants
            .lock()
            .unwrap()
            .insert("own-out".into(), vec!["old-0".into()]);
    }
}

#[tokio::test]
async fn output_sync_updates_unbound_computer_throttles_and_notifies_only_on_change() {
    let f = Fixture::new().await;
    f.store.unbind(f.tenant, f.input).await.unwrap();
    f.only_target();
    f.mock.environments.lock().unwrap().insert(
        "old-0".into(),
        json!({
            "env_id":"old-0", "display_name":"  ", "name":" Renamed Mac ", "host_name":"fallback",
            "online":false, "last_seen_at":"2026-11-11T01:02:03Z"
        }),
    );
    let stop = tokio_util::sync::CancellationToken::new();
    let (control, task) = crate::chains::ChainCoordinator::spawn(
        f.store.clone(),
        f.factory(),
        f.tenant,
        stop.clone(),
    );
    let mut changes = control.subscribe();
    control.sync_output_device(f.output).await.unwrap();
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();
    let record = f.record().await;
    assert_eq!(record.name.as_deref(), Some("Renamed Mac"));
    assert_eq!(record.online, Some(false));
    assert_eq!(record.last_seen_at, Some(1794358923));
    let calls = f.mock.calls.lock().unwrap().len();
    control.sync_output_device(f.output).await.unwrap();
    assert_eq!(f.mock.calls.lock().unwrap().len(), calls);
    assert!(!changes.has_changed().unwrap());
    // Eligible but identical records must not generate an SSE/snapshot refresh loop.
    sqlx::query("UPDATE endpoints SET device_checked_at=NULL")
        .execute(&f.store.pool)
        .await
        .unwrap();
    control.sync_output_device(f.output).await.unwrap();
    assert!(!changes.has_changed().unwrap());
    assert!(f.mock.calls.lock().unwrap().len() > calls);
    let snapshot = serde_json::to_value(
        f.factory()
            .configure(f.tenant, crate::configuration::Command::Snapshot)
            .await
            .unwrap(),
    )
    .unwrap();
    let endpoints = snapshot["value"]["endpoints"].as_array().unwrap();
    let out = endpoints
        .iter()
        .find(|e| e["endpoint"]["device_id"] == f.output.to_string())
        .unwrap();
    assert_eq!(out["target_name"], "Renamed Mac");
    assert_eq!(out["target_online"], false);
    assert_eq!(out["target_last_seen_at"], 1794358923i64);
    let input = endpoints
        .iter()
        .find(|e| e["endpoint"]["device_id"] == f.input.to_string())
        .unwrap();
    assert!(input["target_online"].is_null());
    assert!(input["target_last_seen_at"].is_null());
    stop.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn output_sync_skips_pairing_and_unauthorized_outputs_without_upstream_calls() {
    let f = Fixture::new().await;
    f.store
        .begin_output_pairing(f.tenant, f.output)
        .await
        .unwrap();
    assert!(!f.sync_at(100).await);
    assert!(f.mock.calls.lock().unwrap().is_empty());
    f.pair().await.unwrap();
    f.mock.calls.lock().unwrap().clear();
    sqlx::query("UPDATE endpoints SET lifecycle='draft',device_checked_at=NULL WHERE device_id=?")
        .bind(f.output.to_string())
        .execute(&f.store.pool)
        .await
        .unwrap();
    assert!(!f.sync_at(200).await);
    assert!(f.mock.calls.lock().unwrap().is_empty());
    sqlx::query("UPDATE endpoints SET lifecycle='active' WHERE device_id=?")
        .bind(f.output.to_string())
        .execute(&f.store.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM secrets WHERE tenant_id=? AND scope=?")
        .bind(f.tenant.to_string())
        .bind(crate::vault::scope_key(&SecretScope::OutRegistration(f.output)).unwrap())
        .execute(&f.store.pool)
        .await
        .unwrap();
    assert!(!f.sync_at(230).await);
    assert!(f.mock.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn output_sync_preserves_target_reports_access_anomalies_without_revoking_and_pair_clears_them()
 {
    let f = Fixture::new().await;
    // Both a missing target and extra computers: report the missing target first.
    f.mock
        .grants
        .lock()
        .unwrap()
        .insert("own-out".into(), vec!["other".into()]);
    assert!(f.sync_at(100).await);
    assert_eq!(
        f.record().await.error.as_deref(),
        Some("out_target_missing")
    );
    assert_eq!(f.record().await.name.as_deref(), Some("Old name"));
    f.mock
        .grants
        .lock()
        .unwrap()
        .insert("own-out".into(), vec!["old-0".into(), "other".into()]);
    assert!(f.sync_at(130).await);
    assert_eq!(
        f.record().await.error.as_deref(),
        Some("out_multiple_environments")
    );
    assert_eq!(
        f.store
            .target(f.tenant, f.output)
            .await
            .unwrap()
            .unwrap()
            .environment_id,
        "old-0"
    );
    assert!(
        f.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _)| method != Method::DELETE)
    );
    f.only_target();
    assert!(!f.sync_at(160).await); // Same metadata, hint retained until explicit pairing.
    f.pair().await.unwrap();
    assert_eq!(f.record().await.error, None);
    assert_eq!(
        f.store
            .target(f.tenant, f.output)
            .await
            .unwrap()
            .unwrap()
            .environment_id,
        "new-0"
    );
}

#[tokio::test]
async fn output_sync_read_failures_preserve_record_and_credential_rejection_sets_account_marker() {
    let f = Fixture::new().await;
    f.only_target();
    let before = f.record().await;
    f.mock.fail_list.store(true, Ordering::SeqCst);
    assert!(!f.sync_at(100).await);
    assert_eq!(f.record().await, before);
    let count = f.mock.calls.lock().unwrap().len();
    assert!(!f.sync_at(129).await);
    assert_eq!(f.mock.calls.lock().unwrap().len(), count);
    f.mock.fail_list.store(false, Ordering::SeqCst);
    f.mock.environments.lock().unwrap().insert(
        "old-0".into(),
        json!({"env_id":"old-0","last_seen_at":"invalid"}),
    );
    assert!(!f.sync_at(130).await);
    assert_eq!(f.record().await, before);
    f.mock.reject_account.store(true, Ordering::SeqCst);
    assert!(f.sync_at(160).await);
    let endpoint = f.store.endpoint(f.tenant, f.output).await.unwrap().unwrap();
    assert!(
        f.store
            .sign_in_required(f.tenant, &endpoint.account)
            .await
            .unwrap()
    );
    assert_eq!(f.record().await, before);
}

#[tokio::test]
async fn output_sync_record_hint_and_throttle_survive_database_reopen() {
    let path = std::env::temp_dir().join(format!("out-sync-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let f = Fixture::with_options(options.clone()).await;
    assert!(f.sync_at(100).await);
    let before = f.record().await;
    assert_eq!(before.error.as_deref(), Some("out_multiple_environments"));
    f.store.pool.close().await;
    let reopened = SqliteStore::open(options).await.unwrap();
    assert_eq!(
        reopened.target_record(f.tenant, f.output).await.unwrap(),
        before
    );
    assert!(
        !reopened
            .begin_device_sync(f.tenant, f.output, 129)
            .await
            .unwrap()
    );
    assert!(
        reopened
            .begin_device_sync(f.tenant, f.output, 130)
            .await
            .unwrap()
    );
    reopened.pool.close().await;
    std::fs::remove_file(path).unwrap();
}
