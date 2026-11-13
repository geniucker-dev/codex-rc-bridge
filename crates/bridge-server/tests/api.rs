use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use bridge_core::{
    auth::{AuthService, SecretString},
    chains::{ChainCoordinator, RuntimeFactory},
    identity::{self, TenantSeed},
    management::ManagementAuth,
    model::{AccountIdentity, EndpointKind},
    storage::SqliteStore,
    upstream::{Upstream, unix_time},
    vault::{EncryptionKey, Vault},
};
use bridge_server::api::{self, ApiState};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sqlx::sqlite::SqliteConnectOptions;
use std::time::Duration;
use std::{collections::HashMap, sync::Arc};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

const ORIGIN: &str = "https://bridge.example";
struct Session {
    cookie: String,
    csrf: String,
}
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    session: Option<&Session>,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "bridge.example")
        .header("origin", ORIGIN);
    if let Some(session) = session {
        request = request
            .header("cookie", &session.cookie)
            .header("x-csrf-token", &session.csrf);
    }
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    assert_eq!(parts.headers["cache-control"], "no-store");
    let bytes = to_bytes(body, 65536).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (parts.status, parts.headers, value)
}
async fn login(app: &Router, username: &str) -> (Session, Value) {
    let (status, headers, body) = call(
        app,
        "POST",
        "/api/session",
        None,
        Some(json!({"username":username,"password":"synthetic-password-only"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = headers["set-cookie"].to_str().unwrap();
    for flag in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
        assert!(cookie.contains(flag));
    }
    let session = Session {
        cookie: cookie.split(';').next().unwrap().into(),
        csrf: body["csrf"].as_str().unwrap().into(),
    };
    assert!(
        !body
            .to_string()
            .contains(session.cookie.split('=').nth(1).unwrap())
    );
    (session, body)
}

#[tokio::test]
async fn authenticated_routes_cannot_cross_tenants_or_bypass_session_and_csrf() {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[9; 32]).unwrap());
    let management = Arc::new(ManagementAuth::new(store.clone()));
    let upstream = Upstream::new(None).unwrap();
    let auth = Arc::new(AuthService::new(vault.clone(), upstream.clone()));
    let shutdown = CancellationToken::new();
    let mut controllers = HashMap::new();
    let mut endpoints = Vec::new();
    let mut inputs = Vec::new();
    let mut tenants = Vec::new();
    for name in ["alice", "bob"] {
        let tenant = Uuid::new_v4();
        tenants.push(tenant);
        store.create_tenant(tenant).await.unwrap();
        vault.tenant_seed(tenant).await.unwrap();
        management
            .create_user(
                tenant,
                name,
                SecretString::new("synthetic-password-only".into()),
            )
            .await
            .unwrap();
        let account = AccountIdentity {
            workspace_id: format!("workspace-{name}"),
            user_id: name.into(),
        };
        store.create_account(tenant, &account).await.unwrap();
        let endpoint = identity::endpoint(
            &TenantSeed::from_bytes([7; 32]),
            tenant,
            account.clone(),
            "desk",
            EndpointKind::Out,
        )
        .unwrap();
        store.create_endpoint(&endpoint).await.unwrap();
        endpoints.push(endpoint.device_id);
        let input = identity::endpoint(
            &TenantSeed::from_bytes([7; 32]),
            tenant,
            account,
            "phone",
            EndpointKind::In,
        )
        .unwrap();
        store.create_endpoint(&input).await.unwrap();
        inputs.push(input.device_id);
        let factory = RuntimeFactory {
            store: store.clone(),
            vault: vault.clone(),
            auth: auth.clone(),
            upstream: upstream.clone(),
        };
        let (controller, task) =
            ChainCoordinator::spawn(store.clone(), factory, tenant, shutdown.clone());
        controllers.insert(tenant, (controller, task));
    }
    let api_state = ApiState::new(management.clone(), controllers, ORIGIN).unwrap();
    let app = api::router(api_state.clone());
    assert_eq!(
        call(&app, "GET", "/api/state", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (alice, identity) = login(&app, "alice").await;
    assert_eq!(identity["identity"]["tenant_id"], tenants[0].to_string());
    let (bob, _) = login(&app, "bob").await;
    assert_eq!(
        call(&app, "GET", "/api/session", Some(&alice), None)
            .await
            .0,
        StatusCode::OK
    );
    let state = call(&app, "GET", "/api/state", Some(&alice), None).await;
    assert_eq!(state.0, StatusCode::OK);
    assert_eq!(state.2["session"], identity);
    assert_eq!(state.2["session"]["csrf"], alice.csrf);
    assert!(state.2.to_string().contains("workspace-alice"));
    assert!(!state.2.to_string().contains("workspace-bob"));
    assert_eq!(
        state.2["configuration"]["value"]["accounts"][0]["sign_in_required"],
        false
    );
    let snapshot_endpoints = state.2["configuration"]["value"]["endpoints"]
        .as_array()
        .unwrap();
    assert_eq!(snapshot_endpoints.len(), 2);
    for endpoint in snapshot_endpoints {
        assert_eq!(endpoint["pairing_pending"], false);
        assert_eq!(endpoint["paired_device"], Value::Null);
        assert_eq!(endpoint["pairing_error"], Value::Null);
    }
    let bob_state = call(&app, "GET", "/api/state", Some(&bob), None).await;
    assert_eq!(
        bob_state.2["session"]["identity"]["tenant_id"],
        tenants[1].to_string()
    );
    assert_eq!(bob_state.2["session"]["csrf"], bob.csrf);
    let restore_path = format!("/api/outputs/{}/restore-record", endpoints[1]);
    assert_eq!(
        call(
            &app,
            "POST",
            &restore_path,
            Some(&alice),
            Some(json!({"client_id":"original-client"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &restore_path,
            Some(&alice),
            Some(json!({"client_id":"original-client","tenant_id":tenants[1]}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &restore_path,
            None,
            Some(json!({"client_id":"original-client"}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let no_csrf = Session {
        cookie: alice.cookie.clone(),
        csrf: String::new(),
    };
    assert_eq!(
        call(
            &app,
            "POST",
            &restore_path,
            Some(&no_csrf),
            Some(json!({"client_id":"original-client"}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/outputs/{}/recover", endpoints[1]),
            Some(&alice),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/endpoints/{}", endpoints[1]),
            Some(&alice),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(
        store
            .endpoint(tenants[1], endpoints[1])
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/outputs/{}/authorize", endpoints[1]),
            Some(&alice),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/accounts/login",
            Some(&alice),
            Some(json!({"tenant_id":tenants[1]}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let begun = call(
        &app,
        "POST",
        "/api/accounts/login",
        Some(&alice),
        Some(json!({})),
    )
    .await;
    assert_eq!(begun.0, StatusCode::OK);
    let transaction = begun.2["value"]["id"].as_str().unwrap();
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/logins/{transaction}"),
            Some(&bob),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/logins/{transaction}"),
            Some(&alice),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let invalid = Session {
        cookie: alice.cookie.clone(),
        csrf: "wrong".into(),
    };
    assert_eq!(
        call(&app, "DELETE", "/api/session", Some(&invalid), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let duplicated = Session {
        cookie: format!("{}; {}", alice.cookie, alice.cookie),
        csrf: alice.csrf.clone(),
    };
    assert_eq!(
        call(&app, "GET", "/api/session", Some(&duplicated), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    for (host, origin) in [
        ("attacker.example", ORIGIN),
        ("bridge.example", "https://attacker.example"),
    ] {
        let req = Request::builder()
            .uri("/api/session")
            .header("host", host)
            .header("origin", origin)
            .header("cookie", &alice.cookie)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    let req = Request::builder()
        .method("DELETE")
        .uri("/api/session")
        .header("host", "bridge.example")
        .header("cookie", &alice.cookie)
        .header("x-csrf-token", &alice.csrf)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let req = Request::builder()
        .uri("/api/session")
        .header("host", "bridge.example")
        .header("origin", ORIGIN)
        .header("origin", ORIGIN)
        .header("cookie", &alice.cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    // An established stream is tenant scoped and keeps checking its management session.
    assert_eq!(
        call(&app, "GET", "/api/events", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let mut alice_events = open_events(&app, &alice).await;
    let mut bob_events = open_events(&app, &bob).await;
    assert_eq!(
        event(&mut alice_events).await,
        "event: changed\ndata: {}\n\n"
    );
    assert_eq!(event(&mut bob_events).await, "event: changed\ndata: {}\n\n");
    let invalid_start = format!("/api/inputs/{}/start", Uuid::nil());
    call(&app, "POST", &invalid_start, Some(&alice), None).await;
    assert!(event(&mut alice_events).await.contains("event: changed"));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), bob_events.next())
            .await
            .is_err()
    );
    // Per-tenant limits do not consume another tenant's capacity; dropping a body releases it.
    let mut extra = Vec::new();
    for _ in 0..7 {
        extra.push(open_events(&app, &alice).await);
    }
    assert_eq!(
        call(&app, "GET", "/api/events", Some(&alice), None).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(extra);
    drop(open_events(&app, &alice).await);
    assert_eq!(
        call(&app, "DELETE", "/api/session", Some(&alice), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "GET", "/api/state", Some(&alice), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", "/api/state", Some(&bob), None).await.0,
        StatusCode::OK
    );
    assert!(event(&mut alice_events).await.contains("session_expired"));
    assert!(alice_events.next().await.is_none());
    management
        .reset_password(
            tenants[1],
            "bob",
            SecretString::new("another-synthetic-password".into()),
        )
        .await
        .unwrap();
    assert!(event(&mut bob_events).await.contains("session_expired"));
    assert!(bob_events.next().await.is_none());
    let (alice_again, _) = login(&app, "alice").await;
    let mut shutdown_events = open_events(&app, &alice_again).await;
    event(&mut shutdown_events).await;
    shutdown.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), shutdown_events.next())
            .await
            .unwrap()
            .is_none()
    );
    api_state.shutdown_spaces().await.unwrap();
}

#[tokio::test]
async fn only_https_or_loopback_origins_are_accepted() {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let auth = Arc::new(ManagementAuth::new(store));
    for origin in [
        "http://localhost:8787",
        "http://127.0.0.1:8787",
        "http://[::1]:8787",
        "https://bridge.example",
    ] {
        assert!(ApiState::new(auth.clone(), HashMap::new(), origin).is_ok());
    }
    for origin in [
        "http://bridge.example",
        "https://user:pass@bridge.example",
        "https://bridge.example/path",
        "https://bridge.example/?query=x",
        "file:///tmp/test",
    ] {
        assert!(ApiState::new(auth.clone(), HashMap::new(), origin).is_err());
    }
}

async fn open_events(app: &Router, session: &Session) -> axum::body::BodyDataStream {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/events")
                .header("host", "bridge.example")
                .header("origin", ORIGIN)
                .header("cookie", &session.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.headers()["cache-control"], "no-store");
    response.into_body().into_data_stream()
}
async fn event(stream: &mut axum::body::BodyDataStream) -> String {
    let bytes = tokio::time::timeout(Duration::from_secs(6), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// Transport contract only: the shared core tests exercise real revocation and pairing.
#[derive(Clone)]
struct PairContract;
impl bridge_core::chains::ChainFactory for PairContract {
    async fn sync_input_device(
        &self,
        _: Uuid,
        input: Uuid,
    ) -> Result<bool, bridge_core::configuration::ConfigurationError> {
        if input.is_nil() {
            Err(bridge_core::configuration::ConfigurationError::State)
        } else {
            Ok(false)
        }
    }
    async fn sync_output_device(
        &self,
        _: Uuid,
        output: Uuid,
    ) -> Result<bool, bridge_core::configuration::ConfigurationError> {
        if output.is_nil() {
            Err(bridge_core::configuration::ConfigurationError::State)
        } else {
            Ok(false)
        }
    }

    async fn prepare(
        &self,
        _: bridge_core::model::Binding,
        _: CancellationToken,
    ) -> Result<bridge_core::chains::ChainTask, bridge_core::runtime::RuntimeError> {
        Err(bridge_core::runtime::RuntimeError::State)
    }
    async fn configure(
        &self,
        _: Uuid,
        command: bridge_core::configuration::Command,
    ) -> Result<bridge_core::configuration::Reply, bridge_core::configuration::ConfigurationError>
    {
        use bridge_core::{
            configuration::{Command, Reply},
            output_pairing::{PairOutputError, PairedOutput},
        };
        if let Command::CheckAccount { account } = &command {
            let error = if account.user_id == "step-up" {
                bridge_core::auth::AuthError::StepUp
            } else {
                bridge_core::auth::AuthError::SignInRequired
            };
            return Err(bridge_core::upstream::ControlError::Auth(error).into());
        }
        if let Command::PairIn { input } = &command {
            return if input.is_nil() {
                Err(bridge_core::input_pairing::PairInputError::CleanupPending.into())
            } else {
                Ok(Reply::Pairing {
                    code: SecretString::new("SYNTHETIC-CODE".into()),
                    expires_at: 1234,
                })
            };
        }
        let Command::PairOut { code, .. } = command else {
            panic!("unexpected command")
        };
        match code.expose() {
            "NONE-NONE" => Err(PairOutputError::NoEnvironment.into()),
            "MANY-MANY" => Err(PairOutputError::MultipleEnvironments.into()),
            "WAIT-WAIT" => Err(PairOutputError::CleanupPending.into()),
            _ => Ok(Reply::PairedOutput(PairedOutput {
                environment_id: "synthetic-device".into(),
                name: None,
            })),
        }
    }
}
#[tokio::test]
async fn output_pair_http_contract_and_retired_target_routes() {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let tenant = Uuid::new_v4();
    store.create_tenant(tenant).await.unwrap();
    let management = Arc::new(ManagementAuth::new(store.clone()));
    management
        .create_user(
            tenant,
            "alice",
            SecretString::new("synthetic-password-only".into()),
        )
        .await
        .unwrap();
    let account = AccountIdentity {
        workspace_id: "synthetic".into(),
        user_id: "user".into(),
    };
    store.create_account(tenant, &account).await.unwrap();
    let out = identity::endpoint(
        &TenantSeed::from_bytes([8; 32]),
        tenant,
        account,
        "desk",
        EndpointKind::Out,
    )
    .unwrap();
    store.create_endpoint(&out).await.unwrap();
    let stop = CancellationToken::new();
    let (controller, task) = ChainCoordinator::spawn(store, PairContract, tenant, stop.clone());
    let state = ApiState::new(
        management,
        HashMap::from([(tenant, (controller, task))]),
        ORIGIN,
    )
    .unwrap();
    let app = api::router(state.clone());
    let (session, _) = login(&app, "alice").await;
    let value = call(
        &app,
        "POST",
        &format!("/api/inputs/{}/device/sync", Uuid::new_v4()),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(value.0, StatusCode::NO_CONTENT);
    assert_eq!(value.2, Value::Null);
    let error = call(
        &app,
        "POST",
        &format!("/api/inputs/{}/device/sync", Uuid::nil()),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(error.0, StatusCode::CONFLICT);
    assert_eq!(
        error.2["error"],
        "operation_failed_check_authorization_or_state"
    );
    let unauthenticated = call(
        &app,
        "POST",
        &format!("/api/inputs/{}/device/sync", Uuid::new_v4()),
        None,
        None,
    )
    .await;
    assert_eq!(unauthenticated.0, StatusCode::UNAUTHORIZED);
    let value = call(
        &app,
        "POST",
        &format!("/api/outputs/{}/device/sync", Uuid::new_v4()),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(value.0, StatusCode::NO_CONTENT);
    assert_eq!(value.2, Value::Null);
    let error = call(
        &app,
        "POST",
        &format!("/api/outputs/{}/device/sync", Uuid::nil()),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(error.0, StatusCode::CONFLICT);
    assert_eq!(
        error.2["error"],
        "operation_failed_check_authorization_or_state"
    );
    let unauthenticated = call(
        &app,
        "POST",
        &format!("/api/outputs/{}/device/sync", Uuid::new_v4()),
        None,
        None,
    )
    .await;
    assert_eq!(unauthenticated.0, StatusCode::UNAUTHORIZED);
    let result = call(
        &app,
        "POST",
        &format!("/api/inputs/{}/pair", Uuid::nil()),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(result.0, StatusCode::CONFLICT);
    assert_eq!(result.2["error"], "in_pairing_cleanup_pending");
    let result = call(
        &app,
        "POST",
        &format!("/api/inputs/{}/pair", Uuid::new_v4()),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(
        result.2,
        json!({"result":"pairing","value":{"code":"SYNTHETIC-CODE","expires_at":1234}})
    );
    for (method, suffix) in [("GET", "clients"), ("POST", "clients/revoke")] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/api/inputs/{}/{suffix}", Uuid::new_v4()))
                    .header("host", "bridge.example")
                    .header("origin", ORIGIN)
                    .header("cookie", &session.cookie)
                    .header("x-csrf-token", &session.csrf)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    for (user, expected) in [
        ("account", "account_sign_in_required"),
        ("step-up", "device_authorization_required"),
    ] {
        let result = call(
            &app,
            "POST",
            "/api/accounts/check",
            Some(&session),
            Some(json!({"account":{"workspace_id":"synthetic","user_id":user}})),
        )
        .await;
        assert_eq!(result.0, StatusCode::CONFLICT);
        assert_eq!(result.2["error"], expected);
    }
    let path = format!("/api/outputs/{}/pair", out.device_id);
    let result = call(
        &app,
        "POST",
        &path,
        Some(&session),
        Some(json!({"code":"ABCD-EFGH"})),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(
        result.2,
        json!({"result":"paired_output","value":{"environment_id":"synthetic-device","name":null}})
    );
    for (code, expected) in [
        ("NONE-NONE", "out_pairing_no_environment"),
        ("MANY-MANY", "out_pairing_multiple_environments"),
        ("WAIT-WAIT", "out_pairing_cleanup_pending"),
    ] {
        let result = call(
            &app,
            "POST",
            &path,
            Some(&session),
            Some(json!({"code":code})),
        )
        .await;
        assert_eq!(result.0, StatusCode::CONFLICT);
        assert_eq!(result.2["error"], expected);
    }
    assert_eq!(
        call(&app, "POST", &path, None, Some(json!({"code":"ABCD-EFGH"})))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let no_csrf = Session {
        cookie: session.cookie.clone(),
        csrf: String::new(),
    };
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            Some(&no_csrf),
            Some(json!({"code":"ABCD-EFGH"}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            Some(&session),
            Some(json!({"code":"ABCD-EFGH","tenant_id":Uuid::new_v4()}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for (method, suffix) in [("GET", "targets"), ("POST", "target")] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/api/outputs/{}/{suffix}", out.device_id))
                    .header("host", "bridge.example")
                    .header("origin", ORIGIN)
                    .header("cookie", &session.cookie)
                    .header("x-csrf-token", &session.csrf)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    stop.cancel();
    state.shutdown_spaces().await.unwrap();
}

#[tokio::test]
async fn web_users_dynamic_lifecycle_passwords_permissions_and_streams() {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[23; 32]).unwrap());
    let management = Arc::new(ManagementAuth::new(store.clone()));
    let tenant = Uuid::new_v4();
    vault.create_tenant_with_seed(tenant).await.unwrap();
    management
        .create_user(
            tenant,
            "administrator",
            SecretString::new("synthetic-password-only".into()),
        )
        .await
        .unwrap();
    let upstream = Upstream::new(None).unwrap();
    let factory = RuntimeFactory {
        store: store.clone(),
        vault: vault.clone(),
        auth: Arc::new(AuthService::new(vault.clone(), upstream.clone())),
        upstream: upstream.clone(),
    };
    let stop = CancellationToken::new();
    let state = ApiState::new(management.clone(), HashMap::new(), ORIGIN)
        .unwrap()
        .with_network(vault, upstream)
        .with_runtime(factory, stop.clone());
    state.register_tenant(tenant).await.unwrap();
    let app = api::router(state.clone());
    let (admin, identity) = login(&app, "administrator").await;
    let admin_id = identity["identity"]["user_id"].as_str().unwrap();
    for (body, expected, code) in [
        (
            json!({"username":"BAD", "password":"synthetic-password-only"}),
            StatusCode::BAD_REQUEST,
            "invalid_username",
        ),
        (
            json!({"username":"bob", "password":"short"}),
            StatusCode::BAD_REQUEST,
            "invalid_password",
        ),
    ] {
        let (status, _, body) = call(&app, "POST", "/api/users", Some(&admin), Some(body)).await;
        assert_eq!(status, expected);
        assert_eq!(body["error"], code);
    }
    let (status, _, created) = call(
        &app,
        "POST",
        "/api/users",
        Some(&admin),
        Some(json!({"username":"bob","password":"synthetic-password-only"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap();
    let (status, _, body) = call(
        &app,
        "POST",
        "/api/users",
        Some(&admin),
        Some(json!({"username":"bob","password":"synthetic-password-only"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "username_exists");
    assert_eq!(
        store.local_tenants().await.unwrap().len(),
        2,
        "duplicate must not leave orphan tenant"
    );
    let (bob, _) = login(&app, "bob").await;
    let (bob_other, _) = login(&app, "bob").await;
    let mut admin_events = open_events(&app, &admin).await;
    let mut bob_events = open_events(&app, &bob).await;
    let mut other_events = open_events(&app, &bob_other).await;
    event(&mut admin_events).await;
    event(&mut bob_events).await;
    event(&mut other_events).await;
    assert_eq!(
        call(&app, "GET", "/api/state", Some(&bob), None).await.0,
        StatusCode::OK
    );
    for (method, path, body) in [
        ("GET", "/api/users".to_owned(), None),
        (
            "POST",
            "/api/users".to_owned(),
            Some(json!({"username":"evil","password":"synthetic-password-only"})),
        ),
        (
            "POST",
            format!("/api/users/{id}/password"),
            Some(json!({"password":"synthetic-password-only"})),
        ),
        ("DELETE", format!("/api/users/{id}"), None),
    ] {
        let (status, _, body) = call(&app, method, &path, Some(&bob), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "deployment_admin_required");
    }
    for (method, suffix, body) in [
        (
            "POST",
            "/password",
            Some(json!({"password":"synthetic-password-only"})),
        ),
        ("DELETE", "", None),
    ] {
        let (status, _, body) = call(
            &app,
            method,
            &format!("/api/users/{admin_id}{suffix}"),
            Some(&admin),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "target_is_admin");
        let (status, _, body) = call(
            &app,
            method,
            &format!("/api/users/{}{suffix}", Uuid::new_v4()),
            Some(&admin),
            None.or_else(|| {
                if method == "POST" {
                    Some(json!({"password":"synthetic-password-only"}))
                } else {
                    None
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "user_not_found");
    }
    let (status, _, body) = call(
        &app,
        "POST",
        "/api/session/password",
        Some(&bob),
        Some(json!({"current":"wrong","password":"replacement-password"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "current_password_incorrect");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/session/password",
            Some(&bob),
            Some(json!({"current":"synthetic-password-only","password":"replacement-password"}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(event(&mut other_events).await.contains("session_expired"));
    assert!(event(&mut bob_events).await.contains("heartbeat"));
    assert_eq!(
        call(&app, "GET", "/api/state", Some(&bob_other), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/users/{id}/password"),
            Some(&admin),
            Some(json!({"password":"synthetic-password-only"}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(event(&mut bob_events).await.contains("session_expired"));
    let (bob, _) = login(&app, "bob").await;
    let mut deleted_events = open_events(&app, &bob).await;
    event(&mut deleted_events).await;
    // The dynamically added coordinator participates in proxy pause/resume.
    let (status, _, body) = call(
        &app,
        "POST",
        "/api/deployment/network",
        Some(&admin),
        Some(json!({"proxy_url":null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["restart_failures"], 0);
    assert_eq!(
        call(&app, "GET", "/api/state", Some(&bob), None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/users/{id}"),
            Some(&admin),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), deleted_events.next())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        call(&app, "GET", "/api/state", Some(&bob), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(store.local_tenants().await.unwrap(), vec![tenant]);
    assert_eq!(
        call(&app, "GET", "/api/state", Some(&admin), None).await.0,
        StatusCode::OK
    );
    assert!(!event(&mut admin_events).await.contains("session_expired"));
    let (_, _, users) = call(&app, "GET", "/api/users", Some(&admin), None).await;
    assert_eq!(users["users"].as_array().unwrap().len(), 1);
    assert_eq!(users["users"][0]["current"], true);
    // Add one more space so shutdown must join a live, dynamically created coordinator.
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/users",
            Some(&admin),
            Some(json!({"username":"charlie","password":"synthetic-password-only"}))
        )
        .await
        .0,
        StatusCode::CREATED
    );
    state.finish_operations().await;
    tokio::time::timeout(Duration::from_secs(2), state.shutdown_spaces())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        call(&app, "GET", "/api/state", Some(&admin), None).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[derive(Clone)]
struct SyntheticRunningChain(Arc<std::sync::atomic::AtomicBool>);
impl bridge_core::chains::ChainFactory for SyntheticRunningChain {
    async fn prepare(
        &self,
        _: bridge_core::model::Binding,
        stop: CancellationToken,
    ) -> Result<bridge_core::chains::ChainTask, bridge_core::runtime::RuntimeError> {
        let active = self.0.clone();
        Ok(Box::pin(async move {
            active.store(true, std::sync::atomic::Ordering::SeqCst);
            stop.cancelled().await;
            active.store(false, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }))
    }
}

#[tokio::test]
async fn web_password_reset_keeps_chain_running_and_delete_joins_it_without_remote_cleanup() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let management = Arc::new(ManagementAuth::new(store.clone()));
    let mut spaces = HashMap::new();
    let mut active_flags = Vec::new();
    let mut bob_id = Uuid::nil();
    for name in ["administrator", "bob"] {
        let tenant = Uuid::new_v4();
        store.create_tenant(tenant).await.unwrap();
        management
            .create_user(
                tenant,
                name,
                SecretString::new("synthetic-password-only".into()),
            )
            .await
            .unwrap();
        let account = AccountIdentity {
            workspace_id: "w".into(),
            user_id: "u".into(),
        };
        store.create_account(tenant, &account).await.unwrap();
        let mut ids = Vec::new();
        for kind in [EndpointKind::In, EndpointKind::Out] {
            let endpoint = identity::endpoint(
                &TenantSeed::from_bytes([41; 32]),
                tenant,
                account.clone(),
                "device",
                kind,
            )
            .unwrap();
            store.create_endpoint(&endpoint).await.unwrap();
            ids.push(endpoint.device_id);
        }
        store
            .bind(&bridge_core::model::Binding {
                tenant_id: tenant,
                in_id: ids[0],
                out_id: ids[1],
            })
            .await
            .unwrap();
        let active = Arc::new(AtomicBool::new(false));
        let (controller, task) = ChainCoordinator::spawn(
            store.clone(),
            SyntheticRunningChain(active.clone()),
            tenant,
            CancellationToken::new(),
        );
        controller.start(ids[0]).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !active.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        spaces.insert(tenant, (controller, task));
        active_flags.push(active);
        if name == "bob" {
            bob_id = management
                .users(Uuid::nil(), unix_time())
                .await
                .unwrap()
                .into_iter()
                .find(|u| u.username == "bob")
                .unwrap()
                .id;
        }
    }
    let state = ApiState::new(management, spaces, ORIGIN).unwrap();
    let app = api::router(state.clone());
    let (admin, _) = login(&app, "administrator").await;
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/users/{bob_id}/password"),
            Some(&admin),
            Some(json!({"password":"replacement-password"}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(active_flags.iter().all(|a| a.load(Ordering::SeqCst)));
    // Synthetic factory has no remote cleanup implementation; deletion must never call it.
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/users/{bob_id}"),
            Some(&admin),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(!active_flags[1].load(Ordering::SeqCst));
    assert!(active_flags[0].load(Ordering::SeqCst));
    state.finish_operations().await;
    state.shutdown_spaces().await.unwrap();
    assert!(!active_flags[0].load(Ordering::SeqCst));
}
