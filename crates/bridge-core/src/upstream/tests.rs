use super::*;
use crate::{
    auth::TokenEndpoint,
    chains::RuntimeFactory,
    configuration::{Command, ConfigurationError, Reply},
    identity,
    lifecycle::{self, DeleteOutcome},
    model::{Endpoint, EndpointKind},
    storage::SqliteStore,
    vault::{EncryptionKey, SecretScope, Vault},
};
use axum::{
    extract::State,
    http::{Method, Uri},
    response::IntoResponse,
    routing::any,
};
use serde_json::json;
use sqlx::sqlite::SqliteConnectOptions;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

struct FailingOAuth;
impl TokenEndpoint for FailingOAuth {
    async fn token(&self, _: &[(&str, &str)]) -> Result<TokenResponse, AuthError> {
        Err(AuthError::Network)
    }
}

#[tokio::test]
async fn http_and_websocket_identify_the_bridge_consistently() {
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = observed.clone();
    let router = axum::Router::new().fallback(any(move |headers: axum::http::HeaderMap| {
        let captured = captured.clone();
        async move {
            captured.lock().unwrap().push((
                headers
                    .get("user-agent")
                    .map(|value| value.to_str().unwrap().to_owned()),
                headers
                    .get("originator")
                    .map(|value| value.to_str().unwrap().to_owned()),
            ));
            (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error":"invalid_request"})),
            )
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(stopped.cancelled_owned())
            .await
            .unwrap();
    });
    let mut upstream = Upstream::new(None).unwrap();
    upstream.token_url = format!("http://{address}/oauth/token");
    let http = upstream.token(&[("grant_type", "synthetic")]).await;
    let websocket = crate::transport::connect(
        crate::wire::Side::Out,
        &format!("ws://{address}/controller"),
        Vec::new(),
        None,
    )
    .await;
    stop.cancel();
    server.await.unwrap();

    assert!(matches!(http, Err(AuthError::OAuthRequest)));
    assert!(matches!(
        websocket,
        Err(crate::transport::TransportError::Handshake {
            side: crate::wire::Side::Out,
            status: 400
        })
    ));
    let expected = user_agent();
    assert!(expected.starts_with("codex-rc-bridge/"));
    assert_eq!(
        *observed.lock().unwrap(),
        vec![
            (Some(expected.clone()), None), // OAuth token requests only carry UA.
            (Some(expected), Some("codex_rc_bridge".into()))
        ]
    );
}

struct Mock {
    status: AtomicUsize,
    enrolls: AtomicUsize,
    identities: std::sync::Mutex<Vec<(String, String)>>,
    clients: std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>,
    calls: std::sync::Mutex<Vec<(Method, String)>>,
    fail_cleanup: AtomicBool,
    refresh_missing: AtomicBool,
    refreshes: AtomicUsize,
}
async fn respond(
    State(mock): State<Arc<Mock>>,
    headers: axum::http::HeaderMap,
    method: Method,
    uri: Uri,
) -> axum::response::Response {
    mock.calls
        .lock()
        .unwrap()
        .push((method.clone(), uri.path().into()));
    if uri.path().ends_with("/server/refresh") {
        mock.refreshes.fetch_add(1, Ordering::SeqCst);
        if mock.refresh_missing.swap(false, Ordering::SeqCst) {
            return StatusCode::NOT_FOUND.into_response();
        }
        let replaced = mock.enrolls.load(Ordering::SeqCst) > 0;
        return axum::Json(json!({"server_id":if replaced {"new-host"} else {"old-host"},"environment_id":if replaced {"new-env"} else {"old-env"},
            "remote_control_token":"synthetic-server-token","expires_at":time::OffsetDateTime::from_unix_timestamp(unix_time()+3600).unwrap().format(&time::format_description::well_known::Rfc3339).unwrap()})).into_response();
    }
    if uri.path().ends_with("/server/enroll") {
        mock.enrolls.fetch_add(1, Ordering::SeqCst);
        mock.identities.lock().unwrap().push((
            headers["x-codex-installation-id"]
                .to_str()
                .unwrap()
                .to_owned(),
            headers["chatgpt-account-id"].to_str().unwrap().to_owned(),
        ));
        let status = mock.status.load(Ordering::SeqCst);
        if status != 200 && status != 2 {
            if status == 1 {
                return (StatusCode::OK, "invalid json").into_response();
            }
            return StatusCode::from_u16(status as u16).unwrap().into_response();
        }
        return axum::Json(json!({"server_id":"new-host","environment_id":"new-env",
            "remote_control_token":if status==2 {""} else {"synthetic-server-token"},"expires_at":time::OffsetDateTime::from_unix_timestamp(unix_time()+3600).unwrap().format(&time::format_description::well_known::Rfc3339).unwrap()})).into_response();
    }
    if let Some(rest) = uri
        .path()
        .strip_prefix("/backend-api/wham/remote/control/environments/")
    {
        let parts: Vec<_> = rest.split('/').collect();
        match parts.as_slice() {
            [environment, "clients"] if method == Method::GET => {
                let clients = mock
                    .clients
                    .lock()
                    .unwrap()
                    .get(*environment)
                    .cloned()
                    .unwrap_or_default();
                return axum::Json(json!({"items":clients.into_iter().map(|id|json!({"client_id":id})).collect::<Vec<_>>(),"cursor":null})).into_response();
            }
            [environment, "clients", client] if method == Method::DELETE => {
                if mock.fail_cleanup.load(Ordering::SeqCst) {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                if let Some(clients) = mock.clients.lock().unwrap().get_mut(*environment) {
                    clients.retain(|id| id != client);
                }
                return StatusCode::NO_CONTENT.into_response();
            }
            _ => {}
        }
    }
    StatusCode::NOT_FOUND.into_response()
}
struct Fixture {
    store: Arc<SqliteStore>,
    vault: Vault,
    auth: AuthService<FailingOAuth>,
    upstream: Upstream,
    input: Endpoint,
    replacing: bool,
    mock: Arc<Mock>,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(replacing: bool) -> Self {
        let store = Arc::new(
            SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
                .await
                .unwrap(),
        );
        let tenant = Uuid::new_v4();
        store.create_tenant(tenant).await.unwrap();
        let account = AccountIdentity {
            workspace_id: "synthetic-workspace".into(),
            user_id: "synthetic-user".into(),
        };
        store.create_account(tenant, &account).await.unwrap();
        let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[31; 32]).unwrap());
        let seed = vault.tenant_seed(tenant).await.unwrap();
        let input =
            identity::endpoint(&seed, tenant, account, "test-input", EndpointKind::In).unwrap();
        store.create_endpoint(&input).await.unwrap();
        if replacing {
            vault
                .save_host_registration(
                    tenant,
                    input.device_id,
                    &HostRegistration {
                        server_id: "old-host".into(),
                        environment_id: "old-env".into(),
                    },
                )
                .await
                .unwrap();
        }
        let mock = Arc::new(Mock {
            status: AtomicUsize::new(200),
            enrolls: AtomicUsize::new(0),
            identities: Default::default(),
            clients: Default::default(),
            calls: Default::default(),
            fail_cleanup: AtomicBool::new(false),
            refresh_missing: AtomicBool::new(false),
            refreshes: AtomicUsize::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = Upstream::local_test(&format!("http://{}", listener.local_addr().unwrap()));
        let app = axum::Router::new()
            .fallback(any(respond))
            .with_state(mock.clone());
        let stop = CancellationToken::new();
        let cancel = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(cancel.cancelled_owned())
                .await
                .unwrap();
        });
        let auth = AuthService::new(vault.clone(), FailingOAuth);
        let fixture = Self {
            store,
            vault,
            auth,
            upstream,
            input,
            replacing,
            mock,
            stop,
            server,
        };
        fixture.credentials(false).await;
        fixture
    }
    async fn credentials(&self, expired: bool) {
        self.vault.write(self.input.tenant_id, &SecretScope::Account(self.input.account.clone()),
            &serde_json::to_vec(&json!({"account":self.input.account,"access":"synthetic-access",
                "refresh":"synthetic-refresh","expires_at":if expired {unix_time()-1} else {unix_time()+3600}})).unwrap()).await.unwrap();
    }
    async fn attempt(&self, upstream: &Upstream) -> Result<HostSession, ControlError> {
        if self.replacing {
            upstream
                .registered_host_session(
                    &self.auth,
                    &self.vault,
                    &self.input,
                    "test/1",
                    Some("old-host"),
                )
                .await
        } else {
            upstream
                .enroll_host(&self.auth, &self.vault, &self.input, "test/1")
                .await
        }
    }
    async fn pending(&self) -> bool {
        self.store
            .enrollment_pending(self.input.tenant_id, self.input.device_id)
            .await
            .unwrap()
    }
    async fn assert_previous_registration(&self) {
        let raw = self
            .vault
            .read(
                self.input.tenant_id,
                &SecretScope::InRegistration(self.input.device_id),
            )
            .await
            .unwrap();
        if self.replacing {
            let saved: HostRegistration = serde_json::from_slice(&raw.unwrap()).unwrap();
            assert_eq!(saved.server_id, "old-host");
            assert_eq!(saved.environment_id, "old-env");
        } else {
            assert!(raw.is_none());
        }
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap();
        self.store.pool.close().await;
    }
}

#[tokio::test]
async fn missing_in_registration_revokes_saved_environment_before_reenroll() {
    let f = Fixture::new(true).await;
    sqlx::query("DELETE FROM secrets WHERE tenant_id=? AND scope=?")
        .bind(f.input.tenant_id.to_string())
        .bind(crate::vault::scope_key(&SecretScope::InRegistration(f.input.device_id)).unwrap())
        .execute(&f.store.pool)
        .await
        .unwrap();
    f.mock.clients.lock().unwrap().extend([
        ("old-env".into(), vec!["old-controller".into()]),
        ("another-env".into(), vec!["other-controller".into()]),
    ]);
    f.mock.fail_cleanup.store(true, Ordering::SeqCst);
    assert!(matches!(
        f.attempt(&f.upstream).await,
        Err(ControlError::Rejected(503))
    ));
    assert_eq!(f.mock.enrolls.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.store
            .host_environment(f.input.tenant_id, f.input.device_id)
            .await
            .unwrap()
            .as_deref(),
        Some("old-env")
    );
    f.mock.fail_cleanup.store(false, Ordering::SeqCst);
    f.mock.status.store(503, Ordering::SeqCst);
    assert!(matches!(
        f.attempt(&f.upstream).await,
        Err(ControlError::Rejected(503))
    ));
    assert!(f.mock.clients.lock().unwrap()["old-env"].is_empty());
    assert_eq!(
        f.store
            .host_environment(f.input.tenant_id, f.input.device_id)
            .await
            .unwrap()
            .as_deref(),
        Some("old-env")
    );
    f.mock.status.store(200, Ordering::SeqCst);
    assert_eq!(f.attempt(&f.upstream).await.unwrap().server_id, "new-host");
    assert_eq!(
        f.store
            .host_environment(f.input.tenant_id, f.input.device_id)
            .await
            .unwrap()
            .as_deref(),
        Some("new-env")
    );
    assert_eq!(
        f.mock.clients.lock().unwrap()["another-env"],
        vec!["other-controller"]
    );
    {
        let calls = f.mock.calls.lock().unwrap();
        let first_enroll = calls
            .iter()
            .position(|(_, path)| path.ends_with("/server/enroll"))
            .unwrap();
        assert!(
            calls[..first_enroll]
                .iter()
                .any(|(method, path)| method == Method::DELETE
                    && path.ends_with("/old-env/clients/old-controller"))
        );
        assert!(
            calls
                .iter()
                .filter(|(_, path)| !path.ends_with("/server/enroll"))
                .all(|(_, path)| path
                    .starts_with("/backend-api/wham/remote/control/environments/old-env/clients"))
        );
    }
    f.close().await;
}

#[tokio::test]
async fn shared_enrollment_flow_refreshes_saved_registration_and_recovers_404() {
    let f = Fixture::new(true).await;
    f.mock.refresh_missing.store(true, Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(
            f.upstream
                .enroll_host(&f.auth, &f.vault, &f.input, "test/1")
                .await
                .unwrap()
                .server_id,
            "new-host"
        );
    }
    assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 2);
    assert_eq!(f.mock.enrolls.load(Ordering::SeqCst), 1);
    let saved: HostRegistration = serde_json::from_slice(
        &f.vault
            .read(
                f.input.tenant_id,
                &SecretScope::InRegistration(f.input.device_id),
            )
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved.server_id, "new-host");
    assert_eq!(saved.environment_id, "new-env");
    f.close().await;
}

#[tokio::test]
async fn definite_enroll_failures_preserve_identity_and_allow_retry() {
    for replacing in [false, true] {
        for status in [0, 2, 400, 401, 403, 404, 422, 429] {
            let f = Fixture::new(replacing).await;
            f.mock
                .status
                .store(if status > 2 { status } else { 200 }, Ordering::SeqCst);
            if status == 0 {
                f.credentials(true).await;
            }
            let refused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let offline =
                Upstream::local_test(&format!("http://{}", refused.local_addr().unwrap()));
            drop(refused);
            let error = f
                .attempt(if status == 2 { &offline } else { &f.upstream })
                .await
                .err()
                .unwrap();
            if matches!(status, 0 | 2 | 401) {
                assert!(matches!(error, ControlError::Auth(AuthError::Network)));
                assert!(error.retryable());
            } else {
                assert!(matches!(error,ControlError::Rejected(code) if code==status as u16));
            }
            assert_eq!(
                f.mock.enrolls.load(Ordering::SeqCst),
                usize::from(status > 2)
            );
            assert!(!f.pending().await);
            f.assert_previous_registration().await;
            f.credentials(false).await;
            f.mock.status.store(200, Ordering::SeqCst);
            let session = f.attempt(&f.upstream).await.unwrap();
            assert_eq!(session.server_id, "new-host");
            assert!(!f.pending().await);
            f.close().await;
        }
    }
}

#[tokio::test]
async fn interrupted_enroll_allows_retry_deletion_and_same_name_recreation() {
    for replacing in [false, true] {
        // Malformed 2xx, timeout, 5xx, local save failure, and lost response.
        for (status, deleting) in [1, 2, 408, 503, 200, 0, 3]
            .into_iter()
            .flat_map(|status| [(status, false), (status, true)])
        {
            let f = Fixture::new(replacing).await;
            f.mock
                .status
                .store(if status == 0 { 200 } else { status }, Ordering::SeqCst);
            if status == 200 {
                sqlx::raw_sql("CREATE TRIGGER fail_host_save BEFORE INSERT ON secrets WHEN NEW.scope LIKE '%in_registration%' BEGIN SELECT RAISE(FAIL,'synthetic save failure'); END")
                    .execute(&f.store.pool).await.unwrap();
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let lost = Upstream::local_test(&format!("http://{}", listener.local_addr().unwrap()));
            let release = CancellationToken::new();
            let peer_release = release.clone();
            if status == 3 {
                lost.network.write().unwrap().client = Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_millis(50))
                    .build()
                    .unwrap();
            }
            let expected_id = f.input.device_id.to_string();
            let expected_account = f.input.account.workspace_id.clone();
            let peer = if matches!(status, 0 | 3) {
                Some(tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut header = Vec::new();
                    while !header.ends_with(b"\r\n\r\n") {
                        header.push(socket.read_u8().await.unwrap());
                    }
                    assert!(header.starts_with(b"POST "));
                    let header = String::from_utf8(header).unwrap().to_ascii_lowercase();
                    assert!(header.contains(&format!("x-codex-installation-id: {expected_id}")));
                    assert!(header.contains(&format!("chatgpt-account-id: {expected_account}")));
                    if status == 3 {
                        peer_release.cancelled().await;
                    }
                    // Leave the client with either a timeout or a lost response.
                }))
            } else {
                drop(listener);
                None
            };
            let error = f
                .attempt(if matches!(status, 0 | 3) {
                    &lost
                } else {
                    &f.upstream
                })
                .await
                .err()
                .unwrap();
            assert!(error.retryable(), "{error}");
            release.cancel();
            if let Some(peer) = peer {
                peer.await.unwrap();
            }
            assert!(!f.pending().await);
            f.assert_previous_registration().await;
            if deleting {
                f.store.begin_delete(&f.input).await.unwrap();
                let auth = AuthService::new(f.vault.clone(), f.upstream.clone());
                assert_eq!(
                    lifecycle::cleanup(&f.vault, &auth, &f.upstream, &f.input)
                        .await
                        .unwrap(),
                    DeleteOutcome::Complete
                );
                f.store.create_endpoint(&f.input).await.unwrap();
            }
            if status == 200 {
                sqlx::raw_sql("DROP TRIGGER fail_host_save")
                    .execute(&f.store.pool)
                    .await
                    .unwrap();
            }
            f.mock.status.store(200, Ordering::SeqCst);
            assert_eq!(f.attempt(&f.upstream).await.unwrap().server_id, "new-host");
            let expected = (
                f.input.device_id.to_string(),
                f.input.account.workspace_id.clone(),
            );
            assert!(
                f.mock
                    .identities
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|identity| identity == &expected)
            );
            f.close().await;
        }
    }
}

#[tokio::test]
async fn create_in_rejection_allows_delete_and_same_name_recreation() {
    let f = Fixture::new(false).await;
    let factory = RuntimeFactory {
        store: f.store.clone(),
        vault: f.vault.clone(),
        auth: Arc::new(AuthService::new(f.vault.clone(), f.upstream.clone())),
        upstream: f.upstream.clone(),
    };
    let create = || Command::CreateIn {
        account: f.input.account.clone(),
        name: "new-input".into(),
        output: None,
    };
    f.mock.status.store(403, Ordering::SeqCst);
    assert!(matches!(
        factory.configure(f.input.tenant_id, create()).await,
        Err(ConfigurationError::Control(ControlError::Rejected(403)))
    ));
    let seed = f.vault.tenant_seed(f.input.tenant_id).await.unwrap();
    let input = identity::endpoint(
        &seed,
        f.input.tenant_id,
        f.input.account.clone(),
        "new-input",
        EndpointKind::In,
    )
    .unwrap();
    assert!(
        !f.store
            .enrollment_pending(input.tenant_id, input.device_id)
            .await
            .unwrap()
    );
    f.store.begin_delete(&input).await.unwrap();
    assert_eq!(
        lifecycle::cleanup(&f.vault, &factory.auth, &f.upstream, &input)
            .await
            .unwrap(),
        DeleteOutcome::Complete
    );
    f.mock.status.store(200, Ordering::SeqCst);
    let Reply::Endpoint(restored) = factory.configure(input.tenant_id, create()).await.unwrap()
    else {
        panic!("missing endpoint")
    };
    assert_eq!(restored.device_id, input.device_id);
    f.close().await;
}
