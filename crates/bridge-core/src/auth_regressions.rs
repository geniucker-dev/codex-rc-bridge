//! Local HTTP regression tests. No real accounts or upstream traffic.
use crate::{
    auth::{AccountTokens, AuthError, AuthService, SecretString, TokenResponse},
    model::AccountIdentity,
    storage::SqliteStore,
    upstream::Upstream,
    vault::{EncryptionKey, SecretScope, Vault},
};
use axum::{
    extract::State,
    http::{StatusCode, Uri},
    response::IntoResponse,
    routing::any,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use sqlx::sqlite::SqliteConnectOptions;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
};
use tokio::sync::Notify;
use uuid::Uuid;
struct Mock {
    status: AtomicU16,
    invalid_grant: AtomicBool,
    oauth_body: std::sync::Mutex<Option<serde_json::Value>>,
    access_rejected: AtomicBool,
    refreshes: AtomicUsize,
    hold: AtomicBool,
    entered: Notify,
    release: Notify,
}
async fn respond(State(m): State<Arc<Mock>>, uri: Uri) -> axum::response::Response {
    if uri.path() == "/oauth/token" {
        m.refreshes.fetch_add(1, Ordering::SeqCst);
        let status = StatusCode::from_u16(m.status.load(Ordering::SeqCst)).unwrap();
        if status.is_success() {
            return axum::Json(
                json!({"access_token":access(),"refresh_token":"synthetic-refresh"}),
            )
            .into_response();
        }
        let body = if m.invalid_grant.load(Ordering::SeqCst) {
            Some(json!({"error":"invalid_grant"}))
        } else {
            m.oauth_body.lock().unwrap().clone()
        };
        return (
            status,
            axum::Json(body.unwrap_or(json!({"message":"gateway unavailable"}))),
        )
            .into_response();
    }
    if m.access_rejected.load(Ordering::SeqCst) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if m.hold.load(Ordering::SeqCst) {
        m.entered.notify_one();
        m.release.notified().await;
    }
    axum::Json(json!({"items":[],"cursor":null})).into_response()
}
fn account() -> AccountIdentity {
    AccountIdentity {
        workspace_id: "synthetic-workspace".into(),
        user_id: "synthetic-user".into(),
    }
}
fn access() -> String {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let a = account();
    let claims = json!({"exp":now+3600,"sub":"synthetic-global","https://api.openai.com/auth":{"chatgpt_account_id":a.workspace_id,"chatgpt_account_user_id":a.user_id,"user_id":"synthetic-global"}});
    format!(
        "e30.{}.synthetic",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    )
}
struct Fixture {
    store: Arc<SqliteStore>,
    auth: Arc<AuthService<Upstream>>,
    upstream: Upstream,
    m: Arc<Mock>,
    tenant: Uuid,
    tokens: AccountTokens,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let m = Arc::new(Mock {
            status: AtomicU16::new(400),
            invalid_grant: AtomicBool::new(true),
            oauth_body: Default::default(),
            access_rejected: AtomicBool::new(false),
            refreshes: AtomicUsize::new(0),
            hold: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .fallback(any(respond))
            .with_state(m.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let store = Arc::new(
            SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
                .await
                .unwrap(),
        );
        let tenant = Uuid::new_v4();
        store.create_tenant(tenant).await.unwrap();
        store.create_account(tenant, &account()).await.unwrap();
        let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[91; 32]).unwrap());
        let tokens = AccountTokens::validate(
            TokenResponse {
                access_token: SecretString::new(access()),
                refresh_token: Some(SecretString::new("synthetic-refresh".into())),
            },
            Some(&account()),
            time::OffsetDateTime::now_utc().unix_timestamp(),
        )
        .unwrap();
        vault
            .write(
                tenant,
                &SecretScope::Account(account()),
                &serde_json::to_vec(&tokens).unwrap(),
            )
            .await
            .unwrap();
        let upstream = Upstream::local_test(&origin);
        let auth = Arc::new(AuthService::new(vault, upstream.clone()));
        Self {
            store,
            auth,
            upstream,
            m,
            tenant,
            tokens,
            server,
        }
    }
    async fn force(&self) -> Result<AccountTokens, AuthError> {
        self.auth
            .account(
                self.tenant,
                &account(),
                Some(self.tokens.access_token()),
                time::OffsetDateTime::now_utc().unix_timestamp(),
            )
            .await
    }
}
#[tokio::test]
async fn only_explicit_credential_rejection_marks_sign_in() {
    let f = Fixture::new().await;
    for status in [400, 401, 403, 404, 408, 429, 500, 503] {
        f.m.status.store(status, Ordering::SeqCst);
        f.m.invalid_grant.store(false, Ordering::SeqCst);
        assert!(matches!(f.force().await, Err(AuthError::Network)));
        assert!(
            !f.store
                .sign_in_required(f.tenant, &account())
                .await
                .unwrap()
        );
    }
    f.m.status.store(400, Ordering::SeqCst);
    f.m.invalid_grant.store(true, Ordering::SeqCst);
    assert!(matches!(f.force().await, Err(AuthError::SignInRequired)));
    assert!(
        f.store
            .sign_in_required(f.tenant, &account())
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn marked_account_check_must_refresh_even_when_access_token_is_fresh() {
    let f = Fixture::new().await;
    assert!(f.force().await.is_err());
    assert!(
        f.upstream
            .check_account(&f.auth, f.tenant, &account())
            .await
            .is_err()
    );
    assert_eq!(f.m.refreshes.load(Ordering::SeqCst), 2);
    assert!(
        f.store
            .sign_in_required(f.tenant, &account())
            .await
            .unwrap()
    );
    f.m.status.store(200, Ordering::SeqCst);
    f.upstream
        .check_account(&f.auth, f.tenant, &account())
        .await
        .unwrap();
    assert_eq!(f.m.refreshes.load(Ordering::SeqCst), 3);
    assert!(
        !f.store
            .sign_in_required(f.tenant, &account())
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn old_check_cannot_erase_a_later_failure() {
    let f = Fixture::new().await;
    f.m.hold.store(true, Ordering::SeqCst);
    let a = f.auth.clone();
    let u = f.upstream.clone();
    let tenant = f.tenant;
    let check = tokio::spawn(async move { u.check_account(&a, tenant, &account()).await });
    f.m.entered.notified().await;
    assert!(matches!(f.force().await, Err(AuthError::SignInRequired)));
    f.m.release.notify_one();
    assert!(check.await.unwrap().is_err());
    assert!(
        f.store
            .sign_in_required(f.tenant, &account())
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn unsupported_versions_and_unversioned_nonempty_databases_are_rejected() {
    for version in [1, 7, 10, 11, 12, 13, 15] {
        let path = std::env::temp_dir().join(format!("bridge-schema-{}.sqlite", Uuid::new_v4()));
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = sqlx::SqlitePool::connect_with(options.clone())
            .await
            .unwrap();
        sqlx::query(&format!("PRAGMA user_version={version}"))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        assert!(
            SqliteStore::open(options)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("数据库结构不受支持，需要手工处理")
        );
        std::fs::remove_file(path).unwrap();
    }
    let path = std::env::temp_dir().join(format!("bridge-schema-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let store = SqliteStore::open(options.clone()).await.unwrap();
    store.close().await;
    let reopened = SqliteStore::open(options.clone()).await.unwrap();
    sqlx::query("PRAGMA user_version=0")
        .execute(&reopened.pool)
        .await
        .unwrap();
    reopened.close().await;
    assert!(
        SqliteStore::open(options)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("数据库结构不受支持，需要手工处理")
    );
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn missing_registration_is_distinct_from_corrupt_or_unreadable_storage() {
    use crate::runtime::{RuntimeError, load_registration};
    let f = Fixture::new().await;
    let id = Uuid::new_v4();
    let scope = SecretScope::OutRegistration(id);
    let vault = Vault::new(
        f.store.clone(),
        EncryptionKey::from_bytes(&[91; 32]).unwrap(),
    );
    assert!(matches!(
        load_registration::<serde_json::Value>(&vault, f.tenant, scope.clone()).await,
        Err(RuntimeError::Control(
            crate::upstream::ControlError::Registration
        ))
    ));
    assert!(matches!(
        load_registration::<serde_json::Value>(&vault, f.tenant, SecretScope::InRegistration(id))
            .await,
        Err(RuntimeError::Control(
            crate::upstream::ControlError::HostRegistrationMissing
        ))
    ));
    vault
        .write(f.tenant, &scope, b"invalid JSON")
        .await
        .unwrap();
    assert!(matches!(
        load_registration::<serde_json::Value>(&vault, f.tenant, scope.clone()).await,
        Err(RuntimeError::State)
    ));
    let wrong_key = Vault::new(
        f.store.clone(),
        EncryptionKey::from_bytes(&[92; 32]).unwrap(),
    );
    assert!(matches!(
        load_registration::<serde_json::Value>(&wrong_key, f.tenant, scope.clone()).await,
        Err(RuntimeError::State)
    ));
    f.store.pool.close().await;
    assert!(matches!(
        load_registration::<serde_json::Value>(&vault, f.tenant, scope).await,
        Err(RuntimeError::State)
    ));
}

#[tokio::test]
async fn runtime_load_preserves_local_record_errors_and_their_side() {
    use crate::{
        identity,
        model::{Binding, EndpointKind, Target},
        runtime::{BridgeRuntime, RuntimeError, status::FaultSide},
        upstream::HostRegistration,
    };
    let f = Fixture::new().await;
    let vault = Vault::new(
        f.store.clone(),
        EncryptionKey::from_bytes(&[91; 32]).unwrap(),
    );
    let seed = vault.tenant_seed(f.tenant).await.unwrap();
    let input = identity::endpoint(&seed, f.tenant, account(), "in", EndpointKind::In).unwrap();
    let output = identity::endpoint(&seed, f.tenant, account(), "out", EndpointKind::Out).unwrap();
    for ep in [&input, &output] {
        f.store.create_endpoint(ep).await.unwrap();
    }
    sqlx::query("UPDATE endpoints SET lifecycle='active'")
        .execute(&f.store.pool)
        .await
        .unwrap();
    f.store
        .set_target(
            f.tenant,
            output.device_id,
            &Target {
                environment_id: "remote-target".into(),
                app_server_version: "test/1".into(),
            },
        )
        .await
        .unwrap();
    let binding = Binding {
        tenant_id: f.tenant,
        in_id: input.device_id,
        out_id: output.device_id,
    };
    vault
        .save_host_registration(
            f.tenant,
            input.device_id,
            &HostRegistration {
                server_id: "host".into(),
                environment_id: "host-environment".into(),
            },
        )
        .await
        .unwrap();
    let scope = SecretScope::OutRegistration(output.device_id);
    vault
        .write(f.tenant, &scope, b"invalid JSON")
        .await
        .unwrap();
    let error = BridgeRuntime::load_binding(
        f.store.clone(),
        vault.clone(),
        f.auth.clone(),
        f.upstream.clone(),
        binding.clone(),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(error.root(), RuntimeError::State));
    assert_eq!(
        crate::runtime::status::failure(&error).side,
        Some(FaultSide::Device)
    );
    let wrong = Vault::new(
        f.store.clone(),
        EncryptionKey::from_bytes(&[92; 32]).unwrap(),
    );
    let error = BridgeRuntime::load_binding(
        f.store.clone(),
        wrong,
        f.auth.clone(),
        f.upstream.clone(),
        binding,
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(error.root(), RuntimeError::State));
    assert_eq!(
        crate::runtime::status::failure(&error).side,
        Some(FaultSide::Controller)
    );
}

#[tokio::test]
async fn missing_stored_refresh_token_requires_sign_in() {
    assert_unusable_stored_credentials(true).await;
}
#[tokio::test]
async fn mismatched_stored_account_requires_sign_in() {
    assert_unusable_stored_credentials(false).await;
}
async fn assert_unusable_stored_credentials(missing_refresh: bool) {
    use crate::{
        runtime::{
            RuntimeError,
            status::{ConnectionStatus, FaultSide, failure},
        },
        upstream::ControlError,
    };
    let f = Fixture::new().await;
    let vault = Vault::new(
        f.store.clone(),
        EncryptionKey::from_bytes(&[91; 32]).unwrap(),
    );
    let mut stored = serde_json::to_value(&f.tokens).unwrap();
    if missing_refresh {
        stored["refresh"] = serde_json::Value::Null;
    } else {
        stored["account"]["user_id"] = json!("other-member");
    }
    vault
        .write(
            f.tenant,
            &SecretScope::Account(account()),
            &serde_json::to_vec(&stored).unwrap(),
        )
        .await
        .unwrap();
    let error = f
        .auth
        .account(
            f.tenant,
            &account(),
            None,
            time::OffsetDateTime::now_utc().unix_timestamp(),
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(error, AuthError::SignInRequired));
    assert!(
        f.store
            .sign_in_required(f.tenant, &account())
            .await
            .unwrap()
    );
    let factory = crate::chains::RuntimeFactory {
        store: f.store.clone(),
        vault: vault.clone(),
        auth: f.auth.clone(),
        upstream: f.upstream.clone(),
    };
    let snapshot = factory
        .configure(f.tenant, crate::configuration::Command::Snapshot)
        .await
        .unwrap();
    let snapshot = serde_json::to_value(snapshot).unwrap();
    assert_eq!(snapshot["value"]["accounts"][0]["sign_in_required"], true);
    assert_eq!(
        snapshot["value"]["accounts"][0]["user_id"],
        account().user_id
    );
    let revision: i64 = sqlx::query_scalar("SELECT sign_in_revision FROM accounts")
        .fetch_one(&f.store.pool)
        .await
        .unwrap();
    assert_eq!(revision, 1);
    for side in [FaultSide::Controller, FaultSide::Device] {
        let observed =
            failure(&RuntimeError::from(ControlError::Auth(AuthError::SignInRequired)).at(side));
        assert_eq!(observed.connection, ConnectionStatus::SignInRequired);
        assert_eq!(observed.side, Some(side));
    }
    assert_eq!(f.m.refreshes.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn explicit_oauth_errors_stop_without_marking_accounts_or_retrying() {
    use crate::{
        chains::ChainError,
        configuration::ConfigurationError,
        operation_error::OperationError,
        runtime::{
            RuntimeError,
            status::{ConnectionStatus, FaultSide, failure},
        },
        upstream::ControlError,
    };
    let f = Fixture::new().await;
    f.m.invalid_grant.store(false, Ordering::SeqCst);
    for code in [
        "invalid_client",
        "unauthorized_client",
        "invalid_request",
        "unsupported_grant_type",
    ] {
        for status in [400, 401, 403, 404] {
            *f.m.oauth_body.lock().unwrap() = Some(json!({"error":code}));
            f.m.status.store(status, Ordering::SeqCst);
            let error = f.force().await.err().unwrap();
            assert!(!matches!(
                error,
                AuthError::Network | AuthError::SignInRequired
            ));
            assert!(
                !f.store
                    .sign_in_required(f.tenant, &account())
                    .await
                    .unwrap()
            );
            let public: OperationError =
                ChainError::Configuration(ConfigurationError::Auth(error)).into();
            assert_eq!(public.code, "upstream_auth_rejected");
            for side in [FaultSide::Controller, FaultSide::Device] {
                let error =
                    RuntimeError::from(ControlError::Auth(f.force().await.err().unwrap())).at(side);
                assert!(!error.retryable());
                let observed = failure(&error);
                assert_eq!(observed.connection, ConnectionStatus::Failed);
                assert_eq!(observed.side, Some(side));
            }
        }
    }
    for status in [408, 429, 500, 503] {
        for code in ["invalid_grant", "invalid_client"] {
            *f.m.oauth_body.lock().unwrap() = Some(json!({"error":code}));
            f.m.status.store(status, Ordering::SeqCst);
            assert!(matches!(f.force().await, Err(AuthError::Network)));
            assert!(
                !f.store
                    .sign_in_required(f.tenant, &account())
                    .await
                    .unwrap()
            );
        }
    }
}

#[tokio::test]
async fn revoked_refresh_credentials_are_reported_as_sign_in_required() {
    use crate::{
        chains::ChainError,
        configuration::ConfigurationError,
        operation_error::OperationError,
        runtime::{
            RuntimeError,
            status::{ConnectionStatus, FaultSide, failure},
        },
        upstream::ControlError,
    };
    for code in [
        "refresh_token_expired",
        "refresh_token_reused",
        "refresh_token_invalidated",
    ] {
        for body in [
            json!({"error":code}),
            json!({"error":{"code":code}}),
            json!({"code":code}),
        ] {
            let f = Fixture::new().await;
            f.m.invalid_grant.store(false, Ordering::SeqCst);
            f.m.status.store(401, Ordering::SeqCst);
            f.m.access_rejected.store(true, Ordering::SeqCst);
            *f.m.oauth_body.lock().unwrap() = Some(body);
            let error = f
                .upstream
                .check_account(&f.auth, f.tenant, &account())
                .await
                .err()
                .unwrap();
            assert!(
                matches!(error, ControlError::Auth(AuthError::SignInRequired)),
                "{code}: {error}"
            );
            let public: OperationError =
                ChainError::Configuration(ConfigurationError::Control(error)).into();
            assert_eq!(public.code, "account_sign_in_required");
            assert!(
                f.store
                    .sign_in_required(f.tenant, &account())
                    .await
                    .unwrap()
            );
            let before: i64 = sqlx::query_scalar("SELECT sign_in_revision FROM accounts")
                .fetch_one(&f.store.pool)
                .await
                .unwrap();
            assert!(matches!(f.force().await, Err(AuthError::SignInRequired)));
            let after: i64 = sqlx::query_scalar("SELECT sign_in_revision FROM accounts")
                .fetch_one(&f.store.pool)
                .await
                .unwrap();
            assert_eq!(after, before + 1);
            for side in [FaultSide::Controller, FaultSide::Device] {
                let observed = failure(
                    &RuntimeError::from(ControlError::Auth(AuthError::SignInRequired)).at(side),
                );
                assert_eq!(observed.connection, ConnectionStatus::SignInRequired);
                assert_eq!(observed.side, Some(side));
            }
        }
    }
}

#[tokio::test]
async fn nested_oauth_errors_preserve_transient_and_request_error_classification() {
    let f = Fixture::new().await;
    f.m.invalid_grant.store(false, Ordering::SeqCst);
    for status in [400, 401, 403, 404, 408, 429, 500, 503] {
        for code in [
            "invalid_grant",
            "refresh_token_expired",
            "refresh_token_reused",
            "refresh_token_invalidated",
            "invalid_client",
        ] {
            f.m.status.store(status, Ordering::SeqCst);
            *f.m.oauth_body.lock().unwrap() = Some(json!({"error":{"code":code}}));
            let error = f.force().await.err().unwrap();
            if matches!(status, 408 | 429 | 500 | 503) || code == "invalid_client" {
                assert!(matches!(error, AuthError::Network));
            } else {
                assert!(matches!(error, AuthError::SignInRequired));
            }
            assert_eq!(
                f.store
                    .sign_in_required(f.tenant, &account())
                    .await
                    .unwrap(),
                !matches!(status, 408 | 429 | 500 | 503) && code != "invalid_client"
            );
            f.store
                .set_sign_in_required(f.tenant, &account(), false)
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn ordinary_gateway_codes_remain_retryable_without_marking_login() {
    let f = Fixture::new().await;
    f.m.invalid_grant.store(false, Ordering::SeqCst);
    for status in [400, 401, 403, 404] {
        for body in [
            json!({"code":"proxy_forbidden"}),
            json!({"error":{"code":"gateway_denied"}}),
            json!({"code":403}),
            json!({"error":{"code":"invalid_client"}}),
            json!({"code":"invalid_client"}),
        ] {
            f.m.status.store(status, Ordering::SeqCst);
            *f.m.oauth_body.lock().unwrap() = Some(body);
            let error = f.force().await.err().unwrap();
            assert!(matches!(error, AuthError::Network), "{error}");
            assert!(crate::upstream::ControlError::Auth(error).retryable());
            assert!(
                !f.store
                    .sign_in_required(f.tenant, &account())
                    .await
                    .unwrap()
            );
        }
    }
}
