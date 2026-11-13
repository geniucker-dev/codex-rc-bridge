use super::*;
use crate::chains::RuntimeFactory;
use crate::upstream::unix_time;
use crate::{
    auth::LoginPurpose,
    device::Registration,
    identity,
    model::{AccountIdentity, Binding},
    vault::EncryptionKey,
};
use axum::{
    extract::State,
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::json;
use sqlx::sqlite::SqliteConnectOptions;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    task::JoinHandle,
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Mock {
    clients: Mutex<HashMap<String, Vec<String>>>,
    client_details: Mutex<HashMap<String, serde_json::Value>>,
    controllers: Mutex<HashSet<String>>,
    calls: Mutex<Vec<(Method, String)>>,
    fail: AtomicBool,
    not_found: AtomicBool,
    bad_pair: AtomicBool,
    repeat_cursor: AtomicBool,
    reject_account: AtomicBool,
    list_fail: AtomicBool,
}
fn expiration(seconds: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}
async fn handler(State(mock): State<Arc<Mock>>, method: Method, uri: Uri) -> Response {
    let path = uri.path().to_owned();
    mock.calls
        .lock()
        .unwrap()
        .push((method.clone(), path.clone()));
    if path == "/oauth/token" {
        if mock.reject_account.load(Ordering::SeqCst) {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error":"invalid_grant"})),
            )
                .into_response();
        }
        use base64::Engine;
        let claims = json!({"exp":unix_time()+3600,"sub":"synthetic-global","https://api.openai.com/auth":{"chatgpt_account_id":"workspace","chatgpt_account_user_id":"user"}});
        let access = format!(
            "e30.{}.synthetic",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&claims).unwrap())
        );
        return axum::Json(json!({"access_token":access,"refresh_token":"synthetic-refresh"}))
            .into_response();
    }
    if path.ends_with("/server/refresh") || path.ends_with("/server/enroll") {
        return axum::Json(json!({"server_id":"host-in","environment_id":"env-in","remote_control_token":"synthetic-host","expires_at":expiration(unix_time()+3600)})).into_response();
    }
    if path.ends_with("/server/pair") {
        return axum::Json(json!({"server_id":"host-in","environment_id":if mock.bad_pair.load(Ordering::SeqCst){"foreign-env"}else{"env-in"},"manual_pairing_code":"ABCD-EFGH","expires_at":expiration(unix_time()+60)})).into_response();
    }
    let base = "/backend-api/wham/remote/control/";
    let Some(rest) = path.strip_prefix(base) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let parts: Vec<_> = rest.split('/').collect();
    let (environment, client) = match parts.as_slice() {
        ["environments", environment, "clients"] => (Some(*environment), None),
        ["environments", environment, "clients", client] => (Some(*environment), Some(*client)),
        ["clients"] => (None, None),
        ["clients", client] => (None, Some(*client)),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if method == Method::DELETE {
        if mock.fail.load(Ordering::SeqCst) {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if mock.not_found.load(Ordering::SeqCst) {
            return StatusCode::NOT_FOUND.into_response();
        }
        let client = client.unwrap();
        if let Some(environment) = environment {
            mock.clients
                .lock()
                .unwrap()
                .entry(environment.into())
                .or_default()
                .retain(|c| c != client);
        } else {
            mock.controllers.lock().unwrap().remove(client);
        }
        return StatusCode::NO_CONTENT.into_response();
    }
    if method != Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if mock.list_fail.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if mock.reject_account.load(Ordering::SeqCst) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let url = url::Url::parse(&format!("http://local{uri}")).unwrap();
    let offset = url
        .query_pairs()
        .find(|(key, _)| key == "cursor")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let items = if let Some(environment) = environment {
        mock.clients
            .lock()
            .unwrap()
            .get(environment)
            .cloned()
            .unwrap_or_default()
    } else {
        mock.controllers.lock().unwrap().iter().cloned().collect()
    };
    let page: Vec<_> = items
        .iter()
        .skip(offset)
        .take(1)
        .map(|id| mock.client_details.lock().unwrap().get(id).cloned().unwrap_or_else(|| json!({"client_id":id,"display_name":format!("Device {id}"),"platform":"iOS","device_model":"iPad","device_type":"tablet","last_seen_at":"2026-11-08T12:00:00Z","ignored_sensitive_field":"not-for-ui"})))
        .collect();
    let cursor = if mock.repeat_cursor.load(Ordering::SeqCst) {
        Some("repeated".into())
    } else if offset + 1 < items.len() {
        Some((offset + 1).to_string())
    } else {
        None
    };
    axum::Json(json!({"items":page,"cursor":cursor})).into_response()
}
struct Fixture {
    store: Arc<SqliteStore>,
    vault: Vault,
    auth: AuthService<Upstream>,
    upstream: Upstream,
    input: Endpoint,
    output: Endpoint,
    mock: Arc<Mock>,
    stop: CancellationToken,
    server: Option<JoinHandle<()>>,
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
        let mock = Arc::new(Mock::default());
        mock.clients.lock().unwrap().insert(
            "env-in".into(),
            vec!["shared-phone".into(), "tablet".into()],
        );
        mock.clients
            .lock()
            .unwrap()
            .insert("env-other".into(), vec!["shared-phone".into()]);
        mock.controllers
            .lock()
            .unwrap()
            .extend(["own-out".into(), "other-out".into()]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = Upstream::local_test(&format!("http://{}", listener.local_addr().unwrap()));
        let stop = CancellationToken::new();
        let app = axum::Router::new()
            .fallback(any(handler))
            .with_state(mock.clone());
        let shutdown = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        let tenant = Uuid::new_v4();
        store.create_tenant(tenant).await.unwrap();
        let account = AccountIdentity {
            workspace_id: "workspace".into(),
            user_id: "user".into(),
        };
        store.create_account(tenant, &account).await.unwrap();
        let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
        vault.write(tenant,&SecretScope::Account(account.clone()),&serde_json::to_vec(&json!({"account":account,"access":"synthetic-access","refresh":"synthetic-refresh","global_user":null,"auth_user":null,"expires_at":unix_time()+3600})).unwrap()).await.unwrap();
        let seed = vault.tenant_seed(tenant).await.unwrap();
        let input =
            identity::endpoint(&seed, tenant, account.clone(), "phone", EndpointKind::In).unwrap();
        let output =
            identity::endpoint(&seed, tenant, account, "computer", EndpointKind::Out).unwrap();
        store.create_endpoint(&input).await.unwrap();
        store.create_endpoint(&output).await.unwrap();
        store
            .begin_enrollment(tenant, output.device_id)
            .await
            .unwrap();
        vault
            .save_host_registration(
                tenant,
                input.device_id,
                &HostRegistration {
                    server_id: "host-in".into(),
                    environment_id: "env-in".into(),
                },
            )
            .await
            .unwrap();
        vault
            .save_out_registration(
                tenant,
                output.device_id,
                &Registration {
                    client_id: "own-out".into(),
                    account_user_id: "user".into(),
                    key_id: "synthetic-key".into(),
                },
            )
            .await
            .unwrap();
        let auth = AuthService::new(vault.clone(), upstream.clone());
        Self {
            store,
            vault,
            auth,
            upstream,
            input,
            output,
            mock,
            stop,
            server: Some(server),
        }
    }
    fn session(&self) -> HostSession {
        HostSession {
            server_id: "host-in".into(),
            environment_id: "env-in".into(),
            expires_at: expiration(unix_time() + 3600),
            remote_control_token: crate::auth::SecretString::new("synthetic-session".into()),
        }
    }
    async fn cleanup(&self, endpoint: &Endpoint) -> Result<DeleteOutcome, DeleteError> {
        cleanup(&self.vault, &self.auth, &self.upstream, endpoint).await
    }
    async fn close(mut self) {
        self.stop.cancel();
        timeout(Duration::from_secs(3), self.server.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[tokio::test]
async fn missing_in_registration_uses_saved_environment_across_cleanup_retries_and_restart() {
    let path = std::env::temp_dir().join(format!("bridge-in-cleanup-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let mut f =
        Fixture::with_store(Arc::new(SqliteStore::open(options.clone()).await.unwrap())).await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    sqlx::query("DELETE FROM secrets WHERE tenant_id=? AND scope=?")
        .bind(tenant.to_string())
        .bind(scope_key(&SecretScope::InRegistration(input)).unwrap())
        .execute(&f.store.pool)
        .await
        .unwrap();
    f.mock.clients.lock().unwrap().insert(
        "env-in".into(),
        (0..35).map(|n| format!("client-{n}")).collect(),
    );
    f.store.begin_delete(&f.input).await.unwrap();
    assert_eq!(
        f.cleanup(&f.input).await.unwrap(),
        DeleteOutcome::RevocationPending
    );
    assert_eq!(f.mock.clients.lock().unwrap()["env-in"].len(), 3);
    f.mock.fail.store(true, Ordering::SeqCst);
    assert!(f.cleanup(&f.input).await.is_err());
    assert_eq!(
        f.store
            .host_environment(tenant, input)
            .await
            .unwrap()
            .as_deref(),
        Some("env-in")
    );
    assert!(f.store.endpoint_deleting(tenant, input).await.unwrap());
    f.store.pool.close().await;
    f.store = Arc::new(SqliteStore::open(options).await.unwrap());
    f.vault = Vault::new(
        f.store.clone(),
        EncryptionKey::from_bytes(&[19; 32]).unwrap(),
    );
    f.auth = AuthService::new(f.vault.clone(), f.upstream.clone());
    f.mock.fail.store(false, Ordering::SeqCst);
    assert_eq!(f.cleanup(&f.input).await.unwrap(), DeleteOutcome::Complete);
    assert!(f.mock.clients.lock().unwrap()["env-in"].is_empty());
    assert_eq!(
        f.mock.clients.lock().unwrap()["env-other"],
        vec!["shared-phone"]
    );
    assert_eq!(f.mock.controllers.lock().unwrap().len(), 2);
    assert!(f.mock.calls.lock().unwrap().iter().all(|(_, path)| {
        path.starts_with("/backend-api/wham/remote/control/environments/env-in/clients")
    }));
    assert!(f.store.endpoint(tenant, input).await.unwrap().is_none());
    assert!(
        f.store
            .host_environment(tenant, input)
            .await
            .unwrap()
            .is_none()
    );
    f.store.pool.close().await;
    f.close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn active_in_without_either_identity_cannot_be_erased_or_reenrolled() {
    let f = Fixture::new().await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    sqlx::query("DELETE FROM secrets WHERE tenant_id=? AND scope=?")
        .bind(tenant.to_string())
        .bind(scope_key(&SecretScope::InRegistration(input)).unwrap())
        .execute(&f.store.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM host_environments WHERE tenant_id=? AND in_id=?")
        .bind(tenant.to_string())
        .bind(input.to_string())
        .execute(&f.store.pool)
        .await
        .unwrap();
    assert!(matches!(
        f.store.begin_delete(&f.input).await,
        Err(StorageError::Invalid)
    ));
    assert!(matches!(
        f.upstream
            .enroll_host(&f.auth, &f.vault, &f.input, "mock/1")
            .await,
        Err(ControlError::HostRegistrationMissing)
    ));
    assert_eq!(
        f.store.endpoint(tenant, input).await.unwrap(),
        Some(f.input.clone())
    );
    let lifecycle: String =
        sqlx::query_scalar("SELECT lifecycle FROM endpoints WHERE tenant_id=? AND device_id=?")
            .bind(tenant.to_string())
            .bind(input.to_string())
            .fetch_one(&f.store.pool)
            .await
            .unwrap();
    assert_eq!(lifecycle, "active");
    assert!(f.mock.calls.lock().unwrap().is_empty());
    assert_eq!(f.mock.clients.lock().unwrap()["env-in"].len(), 2);
    f.close().await;
}

#[tokio::test]
async fn in_cleanup_completes_with_fresh_pairing_code_and_only_revokes_its_environment() {
    let f = Fixture::new().await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    pair_host(&f.store, &f.upstream, tenant, input, &f.session())
        .await
        .unwrap();
    f.store
        .claim_stream(tenant, input, "retired-stream")
        .await
        .unwrap();
    f.store.begin_delete(&f.input).await.unwrap();
    let before = f.mock.calls.lock().unwrap().len();
    assert!(
        f.upstream
            .host_session(
                &f.auth,
                &f.input,
                Some(&f.session().registration()),
                "mock/1"
            )
            .await
            .is_err()
    );
    assert_eq!(f.mock.calls.lock().unwrap().len(), before);
    assert!(
        pair_host(&f.store, &f.upstream, tenant, input, &f.session())
            .await
            .is_err()
    );
    assert_eq!(f.cleanup(&f.input).await.unwrap(), DeleteOutcome::Complete);
    assert!(f.mock.clients.lock().unwrap()["env-in"].is_empty());
    assert_eq!(
        f.mock.clients.lock().unwrap()["env-other"],
        vec!["shared-phone"]
    );
    assert!(f.mock.controllers.lock().unwrap().contains("own-out"));
    assert!(f.store.endpoint(tenant, input).await.unwrap().is_none());
    assert!(
        f.vault
            .read(tenant, &SecretScope::InRegistration(input))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !f.store
            .claim_stream(tenant, input, "retired-stream")
            .await
            .unwrap()
    );
    let seed = f.vault.tenant_seed(tenant).await.unwrap();
    let restored = identity::endpoint(
        &seed,
        tenant,
        f.input.account.clone(),
        "phone",
        EndpointKind::In,
    )
    .unwrap();
    assert_eq!(restored.device_id, input);
    f.store.create_endpoint(&restored).await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn failed_out_revocation_blocks_recreation_invalidates_callbacks_and_preserves_shared_login()
{
    let f = Fixture::new().await;
    let tenant = f.output.tenant_id;
    let output = f.output.device_id;
    let purpose = LoginPurpose::OutStepUp {
        device_id: output,
        account: f.output.account.clone(),
    };
    let transaction = f
        .auth
        .begin(tenant, purpose.clone(), 1455, unix_time())
        .await
        .unwrap();
    let state = url::Url::parse(&transaction.url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let seed = f.vault.tenant_seed(tenant).await.unwrap();
    let before = identity::signing_key(&seed, tenant, &f.output.account, &f.output.identity_name)
        .unwrap()
        .public_key()
        .to_vec();
    sqlx::query("INSERT INTO output_pairings(tenant_id,out_id) VALUES(?,?)")
        .bind(tenant.to_string())
        .bind(output.to_string())
        .execute(&f.store.pool)
        .await
        .unwrap();
    f.store.begin_delete(&f.output).await.unwrap();
    f.mock.fail.store(true, Ordering::SeqCst);
    assert!(f.cleanup(&f.output).await.is_err());
    assert!(f.store.endpoint_deleting(tenant, output).await.unwrap());
    assert!(
        f.store
            .output_pairing_pending(tenant, output)
            .await
            .unwrap()
    );
    assert!(
        f.vault
            .read(tenant, &SecretScope::OutRegistration(output))
            .await
            .unwrap()
            .is_some()
    );
    assert!(f.store.create_endpoint(&f.output).await.is_err());
    assert!(!f.store.delete_draft(tenant, output).await.unwrap());
    assert!(
        f.store
            .bind(&Binding {
                tenant_id: tenant,
                in_id: f.input.device_id,
                out_id: output
            })
            .await
            .is_err()
    );
    assert!(
        f.auth
            .begin(tenant, purpose, 1455, unix_time())
            .await
            .is_err()
    );
    f.mock.fail.store(false, Ordering::SeqCst);
    assert_eq!(f.cleanup(&f.output).await.unwrap(), DeleteOutcome::Complete);
    assert!(f.mock.controllers.lock().unwrap().contains("other-out"));
    assert!(!f.mock.controllers.lock().unwrap().contains("own-out"));
    assert!(
        !f.store
            .output_pairing_pending(tenant, output)
            .await
            .unwrap()
    );
    f.store.create_endpoint(&f.output).await.unwrap();
    let callback = format!("http://localhost:1455/auth/callback?code=synthetic&state={state}");
    assert!(matches!(
        f.auth
            .complete(tenant, transaction.id, &callback, unix_time())
            .await,
        Err(AuthError::Transaction)
    ));
    let seed = f.vault.tenant_seed(tenant).await.unwrap();
    assert_eq!(
        identity::signing_key(&seed, tenant, &f.output.account, &f.output.identity_name)
            .unwrap()
            .public_key(),
        before
    );
    assert!(
        f.vault
            .read(tenant, &SecretScope::OutRegistration(output))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.auth
            .account(tenant, &f.output.account, None, unix_time())
            .await
            .is_ok()
    );
    assert!(
        f.vault
            .read(tenant, &SecretScope::InRegistration(f.input.device_id))
            .await
            .unwrap()
            .is_some()
    );
    f.close().await;
}

#[tokio::test]
async fn foreign_pairing_metadata_is_rejected_without_blocking_delete() {
    let f = Fixture::new().await;
    f.mock.bad_pair.store(true, Ordering::SeqCst);
    assert!(
        pair_host(
            &f.store,
            &f.upstream,
            f.input.tenant_id,
            f.input.device_id,
            &f.session()
        )
        .await
        .is_err()
    );
    f.store.begin_delete(&f.input).await.unwrap();
    assert_eq!(f.cleanup(&f.input).await.unwrap(), DeleteOutcome::Complete);
    assert!(
        f.store
            .endpoint(f.input.tenant_id, f.input.device_id)
            .await
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn revoke_404_requires_positive_absence_check_and_list_cursor_loops_are_rejected() {
    let f = Fixture::new().await;
    f.store.begin_delete(&f.output).await.unwrap();
    f.mock.not_found.store(true, Ordering::SeqCst);
    assert!(f.cleanup(&f.output).await.is_err());
    f.mock.controllers.lock().unwrap().remove("own-out"); // Earlier success response was lost.
    assert_eq!(f.cleanup(&f.output).await.unwrap(), DeleteOutcome::Complete);
    f.store.begin_delete(&f.input).await.unwrap();
    f.mock.repeat_cursor.store(true, Ordering::SeqCst);
    assert!(matches!(
        f.cleanup(&f.input).await,
        Err(DeleteError::Control(ControlError::Invalid))
    ));
    f.close().await;
}

#[tokio::test]
async fn cleanup_rejects_foreign_or_bound_endpoints_before_remote_effects() {
    let f = Fixture::new().await;
    let mut foreign = f.output.clone();
    foreign.tenant_id = Uuid::new_v4();
    assert!(f.cleanup(&foreign).await.is_err());
    f.store
        .bind(&Binding {
            tenant_id: f.input.tenant_id,
            in_id: f.input.device_id,
            out_id: f.output.device_id,
        })
        .await
        .unwrap();
    f.store.begin_delete(&f.output).await.unwrap();
    assert!(f.cleanup(&f.output).await.is_err());
    assert!(f.mock.calls.lock().unwrap().is_empty());
    f.close().await;
}

#[tokio::test]
async fn uncertain_registration_cannot_be_erased_or_blindly_retried() {
    let f = Fixture::new().await;
    let seed = f.vault.tenant_seed(f.input.tenant_id).await.unwrap();
    let draft = identity::endpoint(
        &seed,
        f.input.tenant_id,
        f.input.account.clone(),
        "unfinished",
        EndpointKind::Out,
    )
    .unwrap();
    f.store.create_endpoint(&draft).await.unwrap();
    f.store
        .begin_enrollment(draft.tenant_id, draft.device_id)
        .await
        .unwrap();
    assert!(
        f.store
            .begin_enrollment(draft.tenant_id, draft.device_id)
            .await
            .is_err()
    );
    assert!(
        f.store
            .delete_draft(draft.tenant_id, draft.device_id)
            .await
            .is_err()
    );
    f.store.begin_delete(&draft).await.unwrap();
    assert_eq!(
        f.cleanup(&draft).await.unwrap(),
        DeleteOutcome::RegistrationUnknown
    );
    assert!(f.mock.calls.lock().unwrap().is_empty());
    f.close().await;
}

#[tokio::test]
async fn checkpointed_out_can_be_revoked_after_lost_finish_and_does_not_touch_other_controllers() {
    let f = Fixture::new().await;
    let tenant = f.output.tenant_id;
    let seed = f.vault.tenant_seed(tenant).await.unwrap();
    let draft = identity::endpoint(
        &seed,
        tenant,
        f.output.account.clone(),
        "checkpointed",
        EndpointKind::Out,
    )
    .unwrap();
    f.store.create_endpoint(&draft).await.unwrap();
    f.store
        .begin_enrollment(tenant, draft.device_id)
        .await
        .unwrap();
    let key = identity::signing_key(&seed, tenant, &draft.account, &draft.identity_name).unwrap();
    let registration = Registration {
        client_id: "unfinished-out".into(),
        account_user_id: "user".into(),
        key_id: crate::device::identity(&key).key_id,
    };
    assert!(
        f.vault
            .save_out_enrollment(tenant, f.input.device_id, &registration)
            .await
            .is_err()
    );
    f.vault
        .save_out_enrollment(tenant, draft.device_id, &registration)
        .await
        .unwrap();
    let other = Registration {
        client_id: "other-out".into(),
        ..registration
    };
    assert!(
        f.vault
            .save_out_enrollment(tenant, draft.device_id, &other)
            .await
            .is_err()
    );
    assert!(
        f.vault
            .save_out_enrollment(Uuid::new_v4(), draft.device_id, &other)
            .await
            .is_err()
    );
    f.mock
        .controllers
        .lock()
        .unwrap()
        .insert("unfinished-out".into());
    f.store.begin_delete(&draft).await.unwrap();
    assert!(
        crate::enrollment::recover_controller(&f.vault, &f.auth, &f.upstream, &draft)
            .await
            .is_err()
    );
    f.mock.fail.store(true, Ordering::SeqCst);
    assert!(f.cleanup(&draft).await.is_err());
    assert!(
        f.vault
            .read(tenant, &SecretScope::OutEnrollment(draft.device_id))
            .await
            .unwrap()
            .is_some()
    );
    assert!(f.store.create_endpoint(&draft).await.is_err());
    f.mock.fail.store(false, Ordering::SeqCst);
    // Simulate a prior successful revoke whose response was lost: retry returns 404 and list proves absence.
    f.mock.controllers.lock().unwrap().remove("unfinished-out");
    f.mock.not_found.store(true, Ordering::SeqCst);
    assert_eq!(f.cleanup(&draft).await.unwrap(), DeleteOutcome::Complete);
    assert_eq!(f.mock.controllers.lock().unwrap().len(), 2);
    assert!(f.mock.controllers.lock().unwrap().contains("own-out"));
    assert!(f.mock.controllers.lock().unwrap().contains("other-out"));
    assert!(
        f.vault
            .read(tenant, &SecretScope::OutEnrollment(draft.device_id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.vault
            .read(tenant, &SecretScope::Account(draft.account.clone()))
            .await
            .unwrap()
            .is_some()
    );
    f.store.create_endpoint(&draft).await.unwrap();
    assert!(
        f.vault
            .read(tenant, &SecretScope::OutRegistration(draft.device_id))
            .await
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn check_account_contacts_upstream_even_with_a_fresh_cached_token() {
    use crate::{chains::RuntimeFactory, configuration::Command};
    let f = Fixture::new().await;
    let factory = RuntimeFactory {
        store: f.store.clone(),
        vault: f.vault.clone(),
        auth: Arc::new(AuthService::new(f.vault.clone(), f.upstream.clone())),
        upstream: f.upstream.clone(),
    };
    let check = || Command::CheckAccount {
        account: f.input.account.clone(),
    };
    factory.configure(f.input.tenant_id, check()).await.unwrap();
    assert!(
        f.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(method, path)| method == Method::GET && path.ends_with("/control/clients"))
    );
    f.mock.reject_account.store(true, Ordering::SeqCst);
    let error = factory
        .configure(f.input.tenant_id, check())
        .await
        .err()
        .unwrap();
    let error = crate::operation_error::OperationError::from(
        crate::chains::ChainError::Configuration(error),
    );
    assert_eq!(error.code, "account_sign_in_required");
    assert!(
        f.store
            .sign_in_required(f.input.tenant_id, &f.input.account)
            .await
            .unwrap()
    );
    f.mock.reject_account.store(false, Ordering::SeqCst);
    factory.configure(f.input.tenant_id, check()).await.unwrap();
    assert!(
        !f.store
            .sign_in_required(f.input.tenant_id, &f.input.account)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn in_replacement_commits_cleanup_identity_without_reactivating_deletion() {
    let f = Fixture::new().await;
    f.store.begin_delete(&f.input).await.unwrap();
    f.vault
        .save_host_registration(
            f.input.tenant_id,
            f.input.device_id,
            &HostRegistration {
                server_id: "replacement-host".into(),
                environment_id: "replacement-environment".into(),
            },
        )
        .await
        .unwrap();
    assert!(
        f.store
            .endpoint_deleting(f.input.tenant_id, f.input.device_id)
            .await
            .unwrap()
    );
    let saved: HostRegistration = crate::runtime::load_registration(
        &f.vault,
        f.input.tenant_id,
        SecretScope::InRegistration(f.input.device_id),
    )
    .await
    .unwrap();
    assert_eq!(saved.server_id, "replacement-host");
    assert!(
        f.store
            .is_local_environment("replacement-environment")
            .await
            .unwrap()
    );
    f.close().await;
}

fn pairing_factory(f: &Fixture) -> RuntimeFactory {
    RuntimeFactory {
        store: f.store.clone(),
        vault: f.vault.clone(),
        auth: Arc::new(AuthService::new(f.vault.clone(), f.upstream.clone())),
        upstream: f.upstream.clone(),
    }
}
async fn wait_device(f: &Fixture) {
    timeout(Duration::from_secs(5), async {
        while crate::input_pairing::device(&f.store, f.input.tenant_id, f.input.device_id)
            .await
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn single_input_pair_first_repair_and_conflict_are_scoped() {
    use crate::{chains::ChainCoordinator, configuration::Command};
    let f = Fixture::new().await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    f.mock
        .clients
        .lock()
        .unwrap()
        .insert("env-in".into(), vec![]);
    let stop = CancellationToken::new();
    let (control, worker) =
        ChainCoordinator::spawn(f.store.clone(), pairing_factory(&f), tenant, stop.clone());
    let mut changes = control.subscribe();
    control.configure(Command::PairIn { input }).await.unwrap();
    assert!(
        crate::input_pairing::pending(&f.store, tenant, input)
            .await
            .unwrap()
    );
    f.mock
        .clients
        .lock()
        .unwrap()
        .insert("env-in".into(), vec!["phone".into()]);
    wait_device(&f).await;
    changes.changed().await.unwrap();
    let paired = crate::input_pairing::device(&f.store, tenant, input)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(paired.name.as_deref(), Some("Device phone"));
    assert_eq!(paired.platform.as_deref(), Some("iOS"));
    assert_eq!(paired.device_type.as_deref(), Some("tablet"));
    assert_eq!(paired.last_seen_at, Some(1794139200));
    // A successful pairing still watches the complete code lifetime. Sync must skip it.
    control.sync_input_device(input).await.unwrap();
    let checked: Option<i64> =
        sqlx::query_scalar("SELECT device_checked_at FROM endpoints WHERE device_id=?")
            .bind(input.to_string())
            .fetch_one(&f.store.pool)
            .await
            .unwrap();
    assert_eq!(checked, None);

    assert_eq!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap()
            .unwrap()
            .client_id,
        "phone"
    );
    control.stop(input).await.unwrap();
    let calls = f.mock.calls.lock().unwrap().len();
    let snapshot = pairing_factory(&f)
        .configure(tenant, Command::Snapshot)
        .await
        .unwrap();
    let snapshot = serde_json::to_value(snapshot).unwrap();
    let endpoint = snapshot["value"]["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["endpoint"]["device_id"] == input.to_string())
        .unwrap();
    assert_eq!(endpoint["paired_device"]["client_id"], "phone");
    assert_eq!(endpoint["paired_device"]["device_model"], "iPad");
    assert_eq!(f.mock.calls.lock().unwrap().len(), calls);
    // Re-pair does not issue a code until the old environment is empty.
    f.mock.fail.store(true, Ordering::SeqCst);
    let error = control
        .configure(Command::PairIn { input })
        .await
        .err()
        .unwrap();
    assert_eq!(
        crate::operation_error::OperationError::from(error).code,
        "in_pairing_cleanup_pending"
    );
    let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM input_devices")
        .fetch_one(&f.store.pool)
        .await
        .unwrap();
    assert_eq!(retained, 1);
    assert!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap()
            .is_none()
    );
    f.mock.fail.store(false, Ordering::SeqCst);
    control.configure(Command::PairIn { input }).await.unwrap();
    assert!(f.mock.clients.lock().unwrap()["env-in"].is_empty());
    f.mock
        .clients
        .lock()
        .unwrap()
        .insert("env-in".into(), vec!["new-phone".into()]);
    wait_device(&f).await;
    // A second redemption later in the same validity window invalidates both devices.
    f.mock
        .clients
        .lock()
        .unwrap()
        .get_mut("env-in")
        .unwrap()
        .push("second".into());
    timeout(Duration::from_secs(5), async {
        loop {
            if crate::input_pairing::error(&f.store, tenant, input)
                .await
                .unwrap()
                .as_deref()
                == Some("in_pairing_multiple_devices")
                && f.mock.clients.lock().unwrap()["env-in"].is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.mock.clients.lock().unwrap()["env-other"],
        vec!["shared-phone"]
    );
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
}
#[tokio::test]
async fn stopping_and_deleting_input_join_pairing_trackers() {
    use crate::{chains::ChainCoordinator, configuration::Command};
    let f = Fixture::new().await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    let stop = CancellationToken::new();
    let (control, worker) =
        ChainCoordinator::spawn(f.store.clone(), pairing_factory(&f), tenant, stop.clone());
    control.configure(Command::PairIn { input }).await.unwrap();
    control.stop(input).await.unwrap();
    f.mock
        .clients
        .lock()
        .unwrap()
        .insert("env-in".into(), vec!["late-phone".into()]);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap()
            .is_none()
    );
    control.configure(Command::PairIn { input }).await.unwrap();
    control.delete(input).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM input_pairings")
        .fetch_one(&f.store.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
}
#[tokio::test]
async fn pending_input_pairing_survives_database_restart_without_exposing_old_device() {
    use crate::{chains::ChainCoordinator, configuration::Command};
    let path = std::env::temp_dir().join(format!("bridge-input-pairing-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
    let f = Fixture::with_store(store).await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    f.mock
        .clients
        .lock()
        .unwrap()
        .insert("env-in".into(), vec![]);
    let stop = CancellationToken::new();
    let (control, worker) =
        ChainCoordinator::spawn(f.store.clone(), pairing_factory(&f), tenant, stop.clone());
    control.configure(Command::PairIn { input }).await.unwrap();
    f.mock
        .clients
        .lock()
        .unwrap()
        .insert("env-in".into(), vec!["old".into()]);
    wait_device(&f).await;
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.store.pool.close().await;
    let store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
    assert_eq!(
        crate::input_pairing::device(&store, tenant, input)
            .await
            .unwrap()
            .unwrap()
            .client_id,
        "old"
    );
    let mut f = f;
    f.vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
    f.auth = AuthService::new(f.vault.clone(), f.upstream.clone());
    f.store = store;
    f.mock.fail.store(true, Ordering::SeqCst);
    let stop = CancellationToken::new();
    let (control, worker) =
        ChainCoordinator::spawn(f.store.clone(), pairing_factory(&f), tenant, stop.clone());
    assert!(control.configure(Command::PairIn { input }).await.is_err());
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
    let store = SqliteStore::open(options).await.unwrap();
    assert!(
        crate::input_pairing::pending(&store, tenant, input)
            .await
            .unwrap()
    );
    assert!(
        crate::input_pairing::device(&store, tenant, input)
            .await
            .unwrap()
            .is_none()
    );
    store.close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn issued_input_pairing_resumes_tracking_after_database_reopen() {
    use crate::{chains::ChainCoordinator, configuration::Command};
    let path =
        std::env::temp_dir().join(format!("bridge-pairing-resume-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
    let mut f = Fixture::with_store(store).await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    // Persist a code issuance without a running tracker, as if the process exited here.
    pairing_factory(&f)
        .configure(tenant, Command::PairIn { input })
        .await
        .unwrap();
    f.store.pool.close().await;
    let store = Arc::new(SqliteStore::open(options).await.unwrap());
    assert!(
        crate::input_pairing::pending(&store, tenant, input)
            .await
            .unwrap()
    );
    assert!(
        crate::input_pairing::device(&store, tenant, input)
            .await
            .unwrap()
            .is_none()
    );
    f.vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
    f.auth = AuthService::new(f.vault.clone(), f.upstream.clone());
    f.store = store;
    f.mock
        .clients
        .lock()
        .unwrap()
        .insert("env-in".into(), vec!["after-restart".into()]);
    let stop = CancellationToken::new();
    let (control, worker) =
        ChainCoordinator::spawn(f.store.clone(), pairing_factory(&f), tenant, stop.clone());
    wait_device(&f).await;
    assert!(
        !crate::input_pairing::pending(&f.store, tenant, input)
            .await
            .unwrap()
    );
    assert_eq!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap()
            .unwrap()
            .client_id,
        "after-restart"
    );
    drop(control);
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
    std::fs::remove_file(path).unwrap();
}

async fn sync_due(f: &Fixture) {
    sqlx::query("UPDATE endpoints SET device_checked_at=? WHERE tenant_id=? AND device_id=?")
        .bind(unix_time() - 30)
        .bind(f.input.tenant_id.to_string())
        .bind(f.input.device_id.to_string())
        .execute(&f.store.pool)
        .await
        .unwrap();
}
fn remote_devices(f: &Fixture, ids: &[&str]) {
    f.mock.clients.lock().unwrap().insert(
        "env-in".into(),
        ids.iter().map(|id| id.to_string()).collect(),
    );
}
#[tokio::test]
async fn input_sync_throttles_updates_identity_and_clears_absent_device() {
    use crate::{chains::ChainCoordinator, configuration::Command};
    let f = Fixture::new().await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    remote_devices(&f, &["phone"]);
    let stop = CancellationToken::new();
    let (control, worker) =
        ChainCoordinator::spawn(f.store.clone(), pairing_factory(&f), tenant, stop.clone());
    let mut changes = control.subscribe();
    control.sync_input_device(input).await.unwrap();
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();
    let paired = crate::input_pairing::device(&f.store, tenant, input)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(paired.client_id, "phone");
    assert_eq!(paired.device_type.as_deref(), Some("tablet"));
    assert_eq!(paired.last_seen_at, Some(1794139200));
    remote_devices(&f, &["replacement"]);
    f.mock.client_details.lock().unwrap().insert(
        "replacement".into(),
        json!({
            "client_id":"replacement", "display_name":"New name", "device_model":"MacBook",
            "platform":"macOS", "device_type":"laptop", "last_seen_at":"2026-11-11T01:02:03.456Z"
        }),
    );
    // Fresh data does not emit an event or start another request.
    sqlx::query("UPDATE endpoints SET device_checked_at=? WHERE device_id=?")
        .bind(unix_time())
        .bind(input.to_string())
        .execute(&f.store.pool)
        .await
        .unwrap();
    let calls = f.mock.calls.lock().unwrap().len();
    control.sync_input_device(input).await.unwrap();
    assert_eq!(f.mock.calls.lock().unwrap().len(), calls);
    assert!(!changes.has_changed().unwrap());
    assert_eq!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap()
            .unwrap(),
        paired
    );
    sync_due(&f).await;
    control.sync_input_device(input).await.unwrap();
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();
    let updated = crate::input_pairing::device(&f.store, tenant, input)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.client_id, "replacement");
    assert_eq!(updated.name.as_deref(), Some("New name"));
    assert_eq!(updated.platform.as_deref(), Some("macOS"));
    assert_eq!(updated.device_model.as_deref(), Some("MacBook"));
    assert_eq!(updated.device_type.as_deref(), Some("laptop"));
    assert_eq!(updated.last_seen_at, Some(1794358923));
    // A successful but unchanged listing must not trigger another UI refresh.
    sync_due(&f).await;
    control.sync_input_device(input).await.unwrap();
    assert!(!changes.has_changed().unwrap());
    let calls = f.mock.calls.lock().unwrap().len();
    control.configure(Command::Snapshot).await.unwrap();
    assert_eq!(f.mock.calls.lock().unwrap().len(), calls);
    remote_devices(&f, &[]);
    sync_due(&f).await;
    control.sync_input_device(input).await.unwrap();
    assert!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap()
            .is_none()
    );
    assert!(changes.has_changed().unwrap());
    // Wrong roles, unknown IDs and deleting endpoints never reach the upstream.
    let calls = f.mock.calls.lock().unwrap().len();
    assert!(control.sync_input_device(f.output.device_id).await.is_err());
    assert!(control.sync_input_device(Uuid::new_v4()).await.is_err());
    f.store.begin_delete(&f.input).await.unwrap();
    assert!(control.sync_input_device(input).await.is_err());
    assert_eq!(f.mock.calls.lock().unwrap().len(), calls);
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
}
#[tokio::test]
async fn input_sync_multiple_devices_preserves_record_without_revocation_until_repair() {
    use crate::{chains::ChainCoordinator, configuration::Command};
    let f = Fixture::new().await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    remote_devices(&f, &["original"]);
    let stop = CancellationToken::new();
    let (control, worker) =
        ChainCoordinator::spawn(f.store.clone(), pairing_factory(&f), tenant, stop.clone());
    control.sync_input_device(input).await.unwrap();
    let paired = crate::input_pairing::device(&f.store, tenant, input)
        .await
        .unwrap();
    remote_devices(&f, &["original", "unexpected"]);
    sync_due(&f).await;
    let mut changes = control.subscribe();
    control.sync_input_device(input).await.unwrap();
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();
    assert_eq!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap(),
        paired
    );
    assert_eq!(
        crate::input_pairing::error(&f.store, tenant, input)
            .await
            .unwrap()
            .as_deref(),
        Some("in_multiple_devices")
    );
    assert!(
        !crate::input_pairing::pending(&f.store, tenant, input)
            .await
            .unwrap()
    );
    assert!(
        !f.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(method, _)| method == Method::DELETE)
    );
    assert_eq!(f.mock.clients.lock().unwrap()["env-in"].len(), 2);
    sync_due(&f).await;
    control.sync_input_device(input).await.unwrap();
    assert!(!changes.has_changed().unwrap());
    control.configure(Command::PairIn { input }).await.unwrap();
    assert!(
        crate::input_pairing::error(&f.store, tenant, input)
            .await
            .unwrap()
            .is_none()
    );
    assert!(f.mock.clients.lock().unwrap()["env-in"].is_empty());
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
}
#[tokio::test]
async fn input_sync_failures_preserve_metadata_but_publish_credential_rejection() {
    use crate::chains::ChainCoordinator;
    let f = Fixture::new().await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    remote_devices(&f, &["phone"]);
    let stop = CancellationToken::new();
    let (control, worker) =
        ChainCoordinator::spawn(f.store.clone(), pairing_factory(&f), tenant, stop.clone());
    control.sync_input_device(input).await.unwrap();
    let paired = crate::input_pairing::device(&f.store, tenant, input)
        .await
        .unwrap();
    sync_due(&f).await;
    f.mock.list_fail.store(true, Ordering::SeqCst);
    let changes = control.subscribe();
    control.sync_input_device(input).await.unwrap();
    assert_eq!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap(),
        paired
    );
    assert!(
        !f.store
            .sign_in_required(tenant, &f.input.account)
            .await
            .unwrap()
    );
    assert!(!changes.has_changed().unwrap());
    let calls = f.mock.calls.lock().unwrap().len();
    control.sync_input_device(input).await.unwrap();
    assert_eq!(f.mock.calls.lock().unwrap().len(), calls);
    f.mock.list_fail.store(false, Ordering::SeqCst);
    f.mock.client_details.lock().unwrap().insert(
        "phone".into(),
        json!({"client_id":"phone", "last_seen_at":"invalid date"}),
    );
    sync_due(&f).await;
    control.sync_input_device(input).await.unwrap();
    assert_eq!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap(),
        paired
    );
    f.mock.reject_account.store(true, Ordering::SeqCst);
    sync_due(&f).await;
    control.sync_input_device(input).await.unwrap();
    assert!(
        f.store
            .sign_in_required(tenant, &f.input.account)
            .await
            .unwrap()
    );
    assert_eq!(
        crate::input_pairing::device(&f.store, tenant, input)
            .await
            .unwrap(),
        paired
    );
    assert!(changes.has_changed().unwrap());
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
}
#[tokio::test]
async fn input_sync_cache_and_throttle_survive_database_reopen() {
    use crate::chains::ChainCoordinator;
    let path = std::env::temp_dir().join(format!("bridge-sync-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
    let mut f = Fixture::with_store(store).await;
    let tenant = f.input.tenant_id;
    let input = f.input.device_id;
    remote_devices(&f, &["phone"]);
    pairing_factory(&f)
        .sync_input_device(tenant, input)
        .await
        .unwrap();
    f.store.pool.close().await;
    let store = Arc::new(SqliteStore::open(options).await.unwrap());
    f.vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
    f.auth = AuthService::new(f.vault.clone(), f.upstream.clone());
    f.store = store;
    let persisted = crate::input_pairing::device(&f.store, tenant, input)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.device_type.as_deref(), Some("tablet"));
    assert_eq!(persisted.last_seen_at, Some(1794139200));
    let calls = f.mock.calls.lock().unwrap().len();
    let stop = CancellationToken::new();
    let (control, worker) =
        ChainCoordinator::spawn(f.store.clone(), pairing_factory(&f), tenant, stop.clone());
    control.sync_input_device(input).await.unwrap();
    assert_eq!(f.mock.calls.lock().unwrap().len(), calls);
    stop.cancel();
    worker.await.unwrap().unwrap();
    f.close().await;
    std::fs::remove_file(path).unwrap();
}
