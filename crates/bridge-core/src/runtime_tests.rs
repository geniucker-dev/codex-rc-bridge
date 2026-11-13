//! Full control-plane + runtime integration against an isolated local relay. Synthetic accounts only.
use super::*;
use crate::{
    configuration::{Command, Reply},
    model::AccountIdentity,
    vault::EncryptionKey,
};
use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};
use axum::{
    body::{Body, to_bytes},
    extract::{
        FromRequestParts, State, WebSocketUpgrade,
        ws::{Message as WsMessage, WebSocket},
    },
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sqlx::sqlite::SqliteConnectOptions;
use std::{
    collections::HashMap,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Default)]
struct Mock {
    online: AtomicBool,
    computer_name: StdMutex<Option<String>>,
    omit_online: AtomicBool,
    probes: AtomicUsize,
    refreshes: AtomicUsize,
    out_opens: AtomicUsize,
    in_opens: AtomicUsize,
    in_closed: AtomicUsize,
    pair_calls: AtomicUsize,
    paired: AtomicBool,
    reject_pair_once: AtomicBool,
    reject_pair: AtomicBool,
    token_refreshes: AtomicUsize,
    reject_refresh: StdMutex<Option<String>>,
    stale_registration: AtomicBool,
    fail_refresh_once: AtomicBool,
    refresh_error: AtomicUsize,
    in_reject_once: AtomicUsize,
    known_host_404: AtomicBool,
    host_refreshes: AtomicUsize,
    host_missing: AtomicBool,
    deny_host_enroll: AtomicUsize,
    host_generation: AtomicUsize,
    fail_host_enroll_once: AtomicBool,
    hold_host_enroll: AtomicBool,
    host_enroll_entered: Notify,
    host_enroll_release: Notify,
    lose_enroll_finish: AtomicBool,
    enrolled: AtomicBool,
    foreign_refresh_owner: AtomicBool,
    revokes: AtomicUsize,
    enroll_starts: AtomicUsize,
    enroll_finishes: AtomicUsize,
    expired_enroll_challenge: AtomicBool,
    public: StdMutex<Vec<u8>>,
    identity_hash: StdMutex<String>,
    sessions: StdMutex<HashMap<String, i64>>,
    reply: Notify,
    drop_in: Notify,
    stop: CancellationToken,
}
use tokio::sync::Notify;
fn account(which: &str) -> AccountIdentity {
    AccountIdentity {
        workspace_id: format!("workspace-{which}"),
        user_id: format!("user-{which}"),
    }
}
fn token(which: &str, step: bool) -> String {
    let now = unix_time();
    let mut claims = json!({"exp":now+3600,"sub":format!("global-{which}"),"https://api.openai.com/auth":{"chatgpt_account_id":format!("workspace-{which}"),"chatgpt_account_user_id":format!("user-{which}"),"user_id":format!("global-{which}")}});
    claims["synthetic_nonce"] = json!(Uuid::new_v4());
    if step {
        claims["scope"] = json!("codex.remote_control.enroll");
        claims["iat"] = json!(now);
        claims["pwd_auth_time"] = json!(now * 1000);
    }
    format!(
        "e30.{}.synthetic",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    )
}
fn expiration(exp: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(exp)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}
fn verify(mock: &Mock, proof: &Value) {
    let raw = STANDARD
        .decode(
            proof
                .get("signed_payload_base64")
                .or_else(|| proof.get("signedPayloadBase64"))
                .unwrap()
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let signature = STANDARD
        .decode(
            proof
                .get("signature_der_base64")
                .or_else(|| proof.get("signatureDerBase64"))
                .unwrap()
                .as_str()
                .unwrap(),
        )
        .unwrap();
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, &*mock.public.lock().unwrap())
        .verify(&raw, &signature)
        .unwrap();
    let payload: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(payload["domain"], "codex-device-key-sign-payload/v1");
}
async fn handler(State(mock): State<Arc<Mock>>, request: Request<Body>) -> Response {
    let (mut parts, body) = request.into_parts();
    let path = parts.uri.path().to_owned();
    if parts.headers.contains_key("upgrade") {
        assert_eq!(parts.headers["originator"], "codex_rc_bridge");
        assert_eq!(parts.headers["x-codex-protocol-version"], "3");
        let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &mock)
            .await
            .unwrap();
        if path.ends_with("/control/client") {
            assert_eq!(parts.headers["x-codex-client-id"], "controller-b");
            assert_eq!(parts.headers["chatgpt-account-id"], "workspace-b");
            let token = parts.headers["x-codex-client-session-token"]
                .to_str()
                .unwrap()
                .strip_prefix("Bearer ")
                .unwrap()
                .to_owned();
            mock.out_opens.fetch_add(1, Ordering::SeqCst);
            return upgrade
                .on_upgrade(move |socket| out_socket(mock, socket, token))
                .into_response();
        }
        assert!(path.ends_with("/control/server"));
        let expected = if mock.host_generation.load(Ordering::SeqCst) > 1 {
            "host-a-new"
        } else {
            "host-a"
        };
        assert_eq!(parts.headers["x-codex-server-id"], expected);
        assert_eq!(parts.headers["authorization"], "Bearer host-token");
        let status = mock.in_reject_once.swap(0, Ordering::SeqCst) as u16;
        if status != 0 {
            return (StatusCode::from_u16(status).unwrap(), axum::Json(json!({"detail":
                if mock.known_host_404.load(Ordering::SeqCst) { "Remote app server not found" } else { "unknown rejection" }
            }))).into_response();
        }
        mock.in_opens.fetch_add(1, Ordering::SeqCst);
        return upgrade
            .on_upgrade(move |socket| in_socket(mock, socket))
            .into_response();
    }
    let bytes = to_bytes(body, 2 * 1024 * 1024).await.unwrap();
    if path == "/oauth/token" {
        let form: HashMap<_, _> = url::form_urlencoded::parse(&bytes).into_owned().collect();
        assert_eq!(form["client_id"], crate::auth::OAUTH_CLIENT_ID);
        if form["grant_type"] == "refresh_token" {
            mock.token_refreshes.fetch_add(1, Ordering::SeqCst);
        }
        let code = form.get("code").map(String::as_str).unwrap_or("b");
        let which = if code == "a" || form.get("refresh_token").is_some_and(|r| r.ends_with("-a")) {
            "a"
        } else {
            "b"
        };
        if form["grant_type"] == "refresh_token"
            && mock.reject_refresh.lock().unwrap().as_deref() == Some(which)
        {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error":"invalid_grant"})),
            )
                .into_response();
        }
        let mut access = token(which, code.starts_with("step-"));
        if matches!(
            code,
            "step-stale-b" | "step-wrong-b" | "step-scope-b" | "step-expired-b"
        ) {
            let mut claims: Value = serde_json::from_slice(
                &URL_SAFE_NO_PAD
                    .decode(access.split('.').nth(1).unwrap())
                    .unwrap(),
            )
            .unwrap();
            match code {
                "step-stale-b" => {
                    claims["iat"] = json!(unix_time() - 301);
                }
                "step-wrong-b" => {
                    claims["https://api.openai.com/auth"]["chatgpt_account_user_id"] =
                        json!("another-member");
                }
                "step-scope-b" => {
                    claims["scope"] = json!("openid");
                }
                "step-expired-b" => {
                    claims["exp"] = json!(unix_time() - 1);
                }
                _ => unreachable!(),
            }
            access = format!(
                "e30.{}.synthetic",
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
            );
        }
        return axum::Json(
            json!({"access_token":access,"refresh_token":format!("synthetic-refresh-{which}")}),
        )
        .into_response();
    }
    let value: Value = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    assert_eq!(parts.headers["originator"], "codex_rc_bridge");
    let response = if path.ends_with("/enroll/start") || path.ends_with("/refresh/start") {
        assert_eq!(parts.headers["chatgpt-account-id"], "workspace-b");
        if path.ends_with("/enroll/start") {
            mock.enroll_starts.fetch_add(1, Ordering::SeqCst);
        }
        if path.ends_with("/refresh/start") {
            mock.refreshes.fetch_add(1, Ordering::SeqCst);
            let (status, detail) = match mock.refresh_error.load(Ordering::SeqCst) {
                401 => (401, "Remote-control client enrollment is incomplete"),
                403 => (403, "Remote-control client key material missing"),
                404 => (404, "Remote-control client not found"),
                _ => (0, ""),
            };
            if status != 0 {
                return (
                    StatusCode::from_u16(status).unwrap(),
                    axum::Json(json!({"detail":detail})),
                )
                    .into_response();
            }
            if value["client_id"] != "controller-b" {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(json!({"detail":"Remote-control client not found"})),
                )
                    .into_response();
            }
            if mock.stale_registration.load(Ordering::SeqCst)
                || !mock.enrolled.load(Ordering::SeqCst)
            {
                return (
                    StatusCode::FORBIDDEN,
                    axum::Json(json!({"detail":"Remote-control client has been revoked"})),
                )
                    .into_response();
            }
            if mock.fail_refresh_once.swap(false, Ordering::SeqCst) {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
        let finish = path.trim_end_matches("start").to_owned() + "finish";
        let owner = if path.ends_with("/refresh/start")
            && mock.foreign_refresh_owner.load(Ordering::SeqCst)
        {
            "foreign-user"
        } else {
            "user-b"
        };
        let challenge_expiry = unix_time()
            + if path.ends_with("/enroll/start")
                && mock.expired_enroll_challenge.load(Ordering::SeqCst)
            {
                -1
            } else {
                300
            };
        json!({"client_id":"controller-b","account_user_id":owner,"device_key_challenge":{
            "purpose":"remote_control_client_enrollment","audience":"remote_control_client_enrollment","account_user_id":owner,"client_id":"controller-b",
            "target_origin":"https://chatgpt.com","target_path":finish,"challenge_expires_at":challenge_expiry,"device_identity_hash":mock.identity_hash.lock().unwrap().clone(),
            "nonce":URL_SAFE_NO_PAD.encode([0u8;32]),"challenge_id":"challenge-test","challenge_token":"synthetic-challenge"}})
    } else if path.ends_with("/enroll/finish") || path.ends_with("/refresh/finish") {
        verify(&mock, &value["device_key_proof"]);
        assert_eq!(value["client_id"], "controller-b");
        if path.ends_with("/enroll/finish") {
            assert_eq!(value["device_identity"]["algorithm"], "ecdsa_p256_sha256");
            assert!(value["step_up_token"].is_string());
            mock.enroll_finishes.fetch_add(1, Ordering::SeqCst);
            mock.enrolled.store(true, Ordering::SeqCst);
            if mock.lose_enroll_finish.swap(false, Ordering::SeqCst) {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
        let token = format!("controller-session-{}", Uuid::new_v4());
        let expires = unix_time() + 62;
        mock.sessions.lock().unwrap().insert(token.clone(), expires);
        json!({"client_id":"controller-b","account_user_id":"user-b","remote_control_token":token,"scopes":["remote_control_controller_websocket"],"expires_at":expiration(expires)})
    } else if path.ends_with("/server/enroll") || path.ends_with("/server/refresh") {
        assert_eq!(parts.headers["chatgpt-account-id"], "workspace-a");
        assert_eq!(
            parts.headers["x-codex-installation-id"].to_str().unwrap(),
            value["installation_id"].as_str().unwrap()
        );
        if path.ends_with("/server/enroll") {
            if mock.deny_host_enroll.load(Ordering::SeqCst) != 0 {
                return StatusCode::from_u16(mock.deny_host_enroll.load(Ordering::SeqCst) as u16)
                    .unwrap()
                    .into_response();
            }
            assert_eq!(value["app_server_version"], "mock-codex/1");
            if mock.hold_host_enroll.load(Ordering::SeqCst) {
                mock.host_enroll_entered.notify_one();
                mock.host_enroll_release.notified().await;
            }
            mock.host_generation.fetch_add(1, Ordering::SeqCst);
            mock.host_missing.store(false, Ordering::SeqCst);
            if mock.fail_host_enroll_once.swap(false, Ordering::SeqCst) {
                // The registration was accepted, but the client cannot recover its ID.
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        } else {
            mock.host_refreshes.fetch_add(1, Ordering::SeqCst);
            if mock.host_missing.load(Ordering::SeqCst)
                || value["server_id"]
                    != if mock.host_generation.load(Ordering::SeqCst) > 1 {
                        "host-a-new"
                    } else {
                        "host-a"
                    }
            {
                return StatusCode::NOT_FOUND.into_response();
            }
        }
        let replaced = mock.host_generation.load(Ordering::SeqCst) > 1;
        json!({"server_id":if replaced {"host-a-new"} else {"host-a"},"environment_id":if replaced {"environment-a-new"} else {"environment-a"},"remote_control_token":"host-token","expires_at":expiration(unix_time()+62)})
    } else if path.ends_with("/clients") {
        json!({"items":[], "cursor":null})
    } else if path.ends_with("/server/pair") {
        assert_eq!(parts.headers["authorization"], "Bearer host-token");
        assert!(!parts.headers.contains_key("chatgpt-account-id"));
        assert_eq!(value["manual_code"], true);
        let replaced = mock.host_generation.load(Ordering::SeqCst) > 1;
        json!({"manual_pairing_code":"ABCD-EFGH","server_id":if replaced {"host-a-new"} else {"host-a"},"environment_id":if replaced {"environment-a-new"} else {"environment-a"},"expires_at":expiration(unix_time()+60)})
    } else if path.ends_with("/client/pair") {
        mock.pair_calls.fetch_add(1, Ordering::SeqCst);
        if mock.reject_pair_once.swap(false, Ordering::SeqCst)
            || mock.reject_pair.load(Ordering::SeqCst)
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        assert_eq!(value["client_id"], "controller-b");
        assert_eq!(value["manual_pairing_code"], "ABCD-EFGH");
        mock.paired.store(true, Ordering::SeqCst);
        json!({})
    } else if path == "/backend-api/wham/remote/control/clients/controller-b"
        && parts.method == axum::http::Method::DELETE
    {
        assert_eq!(parts.headers["chatgpt-account-id"], "workspace-b");
        mock.revokes.fetch_add(1, Ordering::SeqCst);
        mock.enrolled.store(false, Ordering::SeqCst);
        json!({})
    } else if path.ends_with("/controller-b/environments") {
        mock.probes.fetch_add(1, Ordering::SeqCst);
        let mut environment = json!({"env_id":"target-b","name":mock.computer_name.lock().unwrap().clone().unwrap_or_else(|| "Mock B".into()),"last_seen_at":"2026-11-11T01:02:03Z","online":mock.online.load(Ordering::SeqCst),"app_server_version":"mock-codex/1"});
        if mock.omit_online.load(Ordering::SeqCst) {
            environment.as_object_mut().unwrap().remove("online");
        }
        json!({"items":if mock.paired.load(Ordering::SeqCst) {vec![environment]} else {vec![]},"cursor":null})
    } else {
        return (StatusCode::NOT_FOUND, "unknown mock route").into_response();
    };
    axum::Json(response).into_response()
}
async fn out_socket(mock: Arc<Mock>, mut socket: WebSocket, token: String) {
    let expiry = *mock.sessions.lock().unwrap().get(&token).unwrap();
    let challenge = json!({"type":"device_key_challenge","purpose":"remote_control_client_websocket","audience":"remote_control_client_websocket",
        "targetOrigin":"https://chatgpt.com","targetPath":"/backend-api/codex/remote/control/client","accountUserId":"user-b","clientId":"controller-b",
        "tokenSha256Base64url":device::digest_url(token.as_bytes()),"tokenExpiresAt":expiry,"scopes":["remote_control_controller_websocket"],
        "nonce":URL_SAFE_NO_PAD.encode([0u8;32]),"sessionId":"socket-test"});
    socket
        .send(WsMessage::Text(challenge.to_string().into()))
        .await
        .unwrap();
    let Some(Ok(WsMessage::Text(proof))) = socket.recv().await else {
        panic!("missing device proof")
    };
    verify(&mock, &serde_json::from_str::<Value>(&proof).unwrap());
    loop {
        let message =
            tokio::select! {_=mock.stop.cancelled()=>break,message=socket.recv()=>message};
        match message {
            Some(Ok(WsMessage::Text(raw))) => {
                let value: Value = serde_json::from_str(&raw).unwrap();
                if value["type"] == "client_closed" {
                    continue;
                }
                let ack = json!({"type":"ack","client_id":"controller-b","env_id":"target-b","stream_id":value["stream_id"],"seq_id":value["seq_id"]});
                if socket
                    .send(WsMessage::Text(ack.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
                if value["type"] == "client_message" {
                    assert_eq!(value["message"]["id"], 9007199254740993u64);
                    let response = json!({"type":"server_message","client_id":"controller-b","env_id":"target-b","stream_id":value["stream_id"],"seq_id":1,"cursor":"b-history","message":{"id":9007199254740993u64,"result":"roundtrip"}});
                    if socket
                        .send(WsMessage::Text(response.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
            Some(Ok(WsMessage::Ping(data))) => {
                if socket.send(WsMessage::Pong(data)).await.is_err() {
                    break;
                }
            }
            Some(Ok(WsMessage::Pong(_))) => {}
            _ => break,
        }
    }
}
async fn in_socket(mock: Arc<Mock>, mut socket: WebSocket) {
    let request = json!({"type":"client_message","client_id":"phone-a","stream_id":"source-a","seq_id":1,"cursor":"a-history","message":{"id":9007199254740993u64,"method":"synthetic"}});
    socket
        .send(WsMessage::Text(request.to_string().into()))
        .await
        .unwrap();
    loop {
        let message = tokio::select! {_=mock.stop.cancelled()=>break,_=mock.drop_in.notified()=>break,message=socket.recv()=>message};
        match message {
            Some(Ok(WsMessage::Text(raw))) => {
                let value: Value = serde_json::from_str(&raw).unwrap();
                assert_eq!(value["client_id"], "phone-a");
                assert_eq!(value["stream_id"], "source-a");
                if value["type"] == "server_message" {
                    assert_eq!(value["message"]["result"], "roundtrip");
                    assert_eq!(value["message"]["id"], 9007199254740993u64);
                    mock.reply.notify_one();
                }
                let ack = json!({"type":"ack","client_id":"phone-a","stream_id":"source-a","seq_id":value["seq_id"]});
                if socket
                    .send(WsMessage::Text(ack.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Some(Ok(WsMessage::Ping(data))) => {
                if socket.send(WsMessage::Pong(data)).await.is_err() {
                    break;
                }
            }
            Some(Ok(WsMessage::Pong(_))) => {}
            _ => break,
        }
    }
    mock.in_closed.fetch_add(1, Ordering::SeqCst);
}
async fn callback(
    control: &crate::chains::ChainController,
    command: Command,
    code: &str,
) -> (Uuid, crate::auth::SecretString) {
    let Reply::Login(begin) = control.configure(command).await.unwrap() else {
        panic!("missing login")
    };
    let state = url::Url::parse(&begin.url)
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    (
        begin.id,
        crate::auth::SecretString::new(format!(
            "http://localhost:1455/auth/callback?code={code}&state={state}"
        )),
    )
}
async fn complete(control: &crate::chains::ChainController, command: Command, code: &str) -> Reply {
    let (transaction, callback) = callback(control, command, code).await;
    control
        .configure(Command::CompleteLogin {
            transaction,
            callback,
        })
        .await
        .unwrap()
}
async fn until(mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(10), async {
        while !condition() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn complete_authorization_pairing_and_live_runtime_with_background_renewal() {
    let mock = Arc::new(Mock::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(any(handler))
        .with_state(mock.clone());
    let shutdown = mock.stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let upstream = Upstream::local_test(&origin);
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let tenant = Uuid::new_v4();
    store.create_tenant(tenant).await.unwrap();
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
    let auth = Arc::new(AuthService::new(vault.clone(), upstream.clone()));
    let stop = CancellationToken::new();
    let factory = crate::chains::RuntimeFactory {
        store: store.clone(),
        vault: vault.clone(),
        auth: auth.clone(),
        upstream: upstream.clone(),
    };
    let (control, running) =
        crate::chains::ChainCoordinator::spawn(store.clone(), factory, tenant, stop.clone());
    for side in ["a", "b"] {
        assert!(matches!(
            complete(
                &control,
                Command::BeginAccountLogin {
                    port: 1455,
                    expected: Some(account(side))
                },
                side
            )
            .await,
            Reply::Account(_)
        ));
    }
    let Reply::Endpoint(output) = control
        .configure(Command::CreateOut {
            account: account("b"),
            name: "desk".into(),
        })
        .await
        .unwrap()
    else {
        panic!("missing output")
    };
    let seed = vault.tenant_seed(tenant).await.unwrap();
    let key = identity::signing_key(&seed, tenant, &output.account, &output.identity_name).unwrap();
    *mock.public.lock().unwrap() = key.public_key().to_vec();
    *mock.identity_hash.lock().unwrap() = device::identity_hash(&key);
    assert!(matches!(
        complete(
            &control,
            Command::BeginOutLogin {
                port: 1455,
                output: output.device_id
            },
            "step-b"
        )
        .await,
        Reply::Endpoint(_)
    ));
    let registration: Registration = load_registration(
        &vault,
        tenant,
        SecretScope::OutRegistration(output.device_id),
    )
    .await
    .unwrap();
    mock.reject_pair_once.store(true, Ordering::SeqCst);
    control
        .configure(Command::PairOut {
            output: output.device_id,
            code: crate::auth::SecretString::new("abcd efgh".into()),
        })
        .await
        .unwrap();
    assert_eq!(mock.pair_calls.load(Ordering::SeqCst), 2);
    assert_eq!(mock.token_refreshes.load(Ordering::SeqCst), 1);
    mock.reject_pair.store(true, Ordering::SeqCst);
    assert!(matches!(
        upstream
            .pair_controller(
                &auth,
                tenant,
                &output.account,
                &registration,
                &crate::auth::SecretString::new("ABCD-EFGH".into())
            )
            .await,
        Err(ControlError::Rejected(401))
    ));
    assert_eq!(mock.pair_calls.load(Ordering::SeqCst), 4);
    assert_eq!(mock.token_refreshes.load(Ordering::SeqCst), 2);
    mock.reject_pair.store(false, Ordering::SeqCst);
    mock.stale_registration.store(true, Ordering::SeqCst);
    assert!(matches!(
        upstream
            .refresh_controller(&auth, tenant, &output.account, &key, &registration)
            .await,
        Err(ControlError::Registration)
    ));
    assert_eq!(mock.token_refreshes.load(Ordering::SeqCst), 2);
    mock.stale_registration.store(false, Ordering::SeqCst);
    for status in [401, 403, 404] {
        mock.refresh_error.store(status, Ordering::SeqCst);
        let before = mock.token_refreshes.load(Ordering::SeqCst);
        let result = upstream
            .refresh_controller(&auth, tenant, &output.account, &key, &registration)
            .await;
        match status {
            401 | 403 => assert!(matches!(result, Err(ControlError::Registration))),
            404 => assert!(matches!(result, Err(ControlError::Registration))),
            _ => unreachable!(),
        }
        assert_eq!(
            mock.token_refreshes.load(Ordering::SeqCst),
            before + usize::from(status == 401)
        );
    }
    mock.refresh_error.store(0, Ordering::SeqCst);
    assert_eq!(
        store
            .target(tenant, output.device_id)
            .await
            .unwrap()
            .unwrap()
            .environment_id,
        "target-b"
    );
    let Reply::Endpoint(input) = control
        .configure(Command::CreateIn {
            account: account("a"),
            name: "phone".into(),
            output: Some(output.device_id),
        })
        .await
        .unwrap()
    else {
        panic!("missing input")
    };
    let Reply::Pairing { code, .. } = control
        .configure(Command::PairIn {
            input: input.device_id,
        })
        .await
        .unwrap()
    else {
        panic!("missing pairing")
    };
    assert_eq!(code.expose(), "ABCD-EFGH");
    control
        .configure(Command::Bind {
            input: input.device_id,
            output: output.device_id,
        })
        .await
        .unwrap();
    *mock.computer_name.lock().unwrap() = Some("Probe renamed".into());
    let mut changes = control.subscribe();
    control.start(input.device_id).await.unwrap();
    until(|| mock.probes.load(Ordering::SeqCst) >= 2).await;
    assert_eq!(mock.in_opens.load(Ordering::SeqCst), 0);
    assert_eq!(mock.out_opens.load(Ordering::SeqCst), 0);
    // A request reaching the mock is not proof that its response was applied.
    wait_connection(&control, ConnectionStatus::TargetOffline).await;
    let probed_record = store.target_record(tenant, output.device_id).await.unwrap();
    assert_eq!(probed_record.name.as_deref(), Some("Probe renamed"));
    assert_eq!(probed_record.online, Some(false));
    assert_eq!(probed_record.last_seen_at, Some(1794358923));
    control.sync_output_device(output.device_id).await.unwrap();
    assert_eq!(
        store.target_record(tenant, output.device_id).await.unwrap(),
        probed_record
    );
    changes.borrow_and_update();
    mock.online.store(true, Ordering::SeqCst);
    timeout(Duration::from_secs(10), mock.reply.notified())
        .await
        .unwrap();
    assert_eq!(
        control.status().await.unwrap()[0].connection,
        ConnectionStatus::Connected
    );
    assert!(changes.has_changed().unwrap());
    let refreshed = mock.refreshes.load(Ordering::SeqCst);
    mock.fail_refresh_once.store(true, Ordering::SeqCst);
    until(|| mock.refreshes.load(Ordering::SeqCst) >= refreshed + 2).await;
    assert_eq!(mock.in_opens.load(Ordering::SeqCst), 1);
    assert_eq!(mock.out_opens.load(Ordering::SeqCst), 1);
    assert_eq!(mock.in_closed.load(Ordering::SeqCst), 0);
    assert_eq!(
        control.status().await.unwrap()[0].connection,
        ConnectionStatus::Connected,
        "background renewal failure must not mark a healthy relay disconnected"
    );
    mock.drop_in.notify_one();
    wait_connection(&control, ConnectionStatus::Reconnecting).await;
    wait_connection(&control, ConnectionStatus::Connected).await;
    assert_eq!(mock.in_opens.load(Ordering::SeqCst), 2);
    // IN uses server tokens: 401/403 renew them without rotating account OAuth.
    // An unknown 404 retries with the existing registration.
    for status in [401, 403, 404] {
        control.stop(input.device_id).await.unwrap();
        let opens = mock.in_opens.load(Ordering::SeqCst);
        let refreshed = mock.host_refreshes.load(Ordering::SeqCst);
        let oauth = mock.token_refreshes.load(Ordering::SeqCst);
        mock.in_reject_once.store(status, Ordering::SeqCst);
        control.start(input.device_id).await.unwrap();
        until(|| mock.in_opens.load(Ordering::SeqCst) > opens).await;
        wait_connection(&control, ConnectionStatus::Connected).await;
        assert!(mock.host_refreshes.load(Ordering::SeqCst) >= refreshed + 2);
        assert_eq!(mock.token_refreshes.load(Ordering::SeqCst), oauth);
        assert_eq!(mock.host_generation.load(Ordering::SeqCst), 1);
    }
    mock.online.store(false, Ordering::SeqCst);
    let opened = mock.in_opens.load(Ordering::SeqCst);
    until(|| mock.in_closed.load(Ordering::SeqCst) == opened).await;
    wait_connection(&control, ConnectionStatus::TargetOffline).await;
    // Lost IN enrollment reconnects using the new server ID and the same installation ID.
    mock.fail_host_enroll_once.store(true, Ordering::SeqCst);
    mock.host_missing.store(true, Ordering::SeqCst);
    until(|| mock.host_generation.load(Ordering::SeqCst) == 3).await;
    let saved: HostRegistration =
        load_registration(&vault, tenant, SecretScope::InRegistration(input.device_id))
            .await
            .unwrap();
    assert_eq!(saved.server_id, "host-a-new");
    assert_eq!(saved.environment_id, "environment-a-new");
    assert!(
        store
            .is_local_environment("environment-a-new")
            .await
            .unwrap()
    );
    mock.online.store(true, Ordering::SeqCst);
    wait_connection(&control, ConnectionStatus::Connected).await;
    assert!(mock.in_opens.load(Ordering::SeqCst) >= 3);
    // A recognized WebSocket 404 replaces enrollment even if HTTP refresh succeeds.
    mock.known_host_404.store(true, Ordering::SeqCst);
    mock.in_reject_once.store(404, Ordering::SeqCst);
    mock.drop_in.notify_one();
    until(|| mock.host_generation.load(Ordering::SeqCst) == 4).await;
    wait_connection(&control, ConnectionStatus::Connected).await;
    // Stop during enroll backoff joins the worker; start resumes with the same identity.
    mock.fail_host_enroll_once.store(true, Ordering::SeqCst);
    mock.in_reject_once.store(404, Ordering::SeqCst);
    mock.drop_in.notify_one();
    until(|| mock.host_generation.load(Ordering::SeqCst) == 5).await;
    control.stop(input.device_id).await.unwrap();
    assert_eq!(
        control.status().await.unwrap()[0].connection,
        ConnectionStatus::Stopped
    );
    assert_eq!(mock.host_generation.load(Ordering::SeqCst), 5);
    let refreshed = mock.host_refreshes.load(Ordering::SeqCst);
    control.start(input.device_id).await.unwrap();
    wait_connection(&control, ConnectionStatus::Connected).await;
    // The mock accepts the saved registration again; recovery need not enroll twice.
    assert!(mock.host_refreshes.load(Ordering::SeqCst) > refreshed);
    assert_eq!(mock.host_generation.load(Ordering::SeqCst), 5);
    // Missing presence metadata adds no evidence, but must not terminate the chain.
    mock.omit_online.store(true, Ordering::SeqCst);
    let probed = mock.probes.load(Ordering::SeqCst);
    until(|| mock.probes.load(Ordering::SeqCst) > probed).await;
    assert_eq!(
        upstream
            .target_online(
                &auth,
                tenant,
                &output.account,
                &registration.client_id,
                "target-b"
            )
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        control.status().await.unwrap()[0].connection,
        ConnectionStatus::Connected
    );
    mock.omit_online.store(false, Ordering::SeqCst);
    mock.online.store(false, Ordering::SeqCst);
    wait_connection(&control, ConnectionStatus::TargetOffline).await;
    mock.stale_registration.store(true, Ordering::SeqCst);
    wait_connection(&control, ConnectionStatus::DeviceAuthorizationRequired).await;
    assert!(
        control.status().await.unwrap()[0].enabled,
        "failure must preserve configured ownership and desired run state"
    );
    assert_eq!(
        control.status().await.unwrap()[0].side,
        Some(FaultSide::Device)
    );
    mock.stale_registration.store(false, Ordering::SeqCst);
    mock.online.store(true, Ordering::SeqCst);
    complete(
        &control,
        Command::BeginOutLogin {
            output: output.device_id,
            port: 1455,
        },
        "step-b",
    )
    .await;
    wait_connection(&control, ConnectionStatus::Connected).await;
    // Reauthorization may lose its finish reply while an existing target remains bound.
    // Recover its checkpoint without creating another identity, then restore the enabled chain.
    mock.stale_registration.store(true, Ordering::SeqCst);
    wait_connection(&control, ConnectionStatus::DeviceAuthorizationRequired).await;
    mock.stale_registration.store(false, Ordering::SeqCst);
    mock.lose_enroll_finish.store(true, Ordering::SeqCst);
    let (transaction, callback) = callback(
        &control,
        Command::BeginOutLogin {
            output: output.device_id,
            port: 1455,
        },
        "step-b",
    )
    .await;
    assert!(
        control
            .configure(Command::CompleteLogin {
                transaction,
                callback
            })
            .await
            .is_err()
    );
    assert!(
        store
            .enrollment_pending(tenant, output.device_id)
            .await
            .unwrap()
    );
    control
        .configure(Command::RecoverOut {
            output: output.device_id,
        })
        .await
        .unwrap();
    wait_connection(&control, ConnectionStatus::Connected).await;
    // Force genuine account-renewal failures independently on both sides.
    for (which, side) in [("a", FaultSide::Controller), ("b", FaultSide::Device)] {
        control.stop(input.device_id).await.unwrap();
        let scope = SecretScope::Account(account(which));
        let raw = vault.read(tenant, &scope).await.unwrap().unwrap();
        let mut expired: Value = serde_json::from_slice(&raw).unwrap();
        expired["expires_at"] = json!(0);
        vault
            .write(tenant, &scope, &serde_json::to_vec(&expired).unwrap())
            .await
            .unwrap();
        *mock.reject_refresh.lock().unwrap() = Some(which.into());
        control.start(input.device_id).await.unwrap();
        wait_connection(&control, ConnectionStatus::SignInRequired).await;
        assert_eq!(control.status().await.unwrap()[0].side, Some(side));
        assert!(
            store
                .sign_in_required(tenant, &account(which))
                .await
                .unwrap()
        );
        assert!(
            !store
                .sign_in_required(tenant, &account(if which == "a" { "b" } else { "a" }))
                .await
                .unwrap()
        );
        let snapshot =
            serde_json::to_value(control.configure(Command::Snapshot).await.unwrap()).unwrap();
        for info in snapshot["value"]["accounts"].as_array().unwrap() {
            assert_eq!(
                info["sign_in_required"],
                info["workspace_id"] == format!("workspace-{which}")
            );
        }
        *mock.reject_refresh.lock().unwrap() = None;
        complete(
            &control,
            Command::BeginAccountLogin {
                expected: Some(account(which)),
                port: 1455,
            },
            which,
        )
        .await;
        wait_connection(&control, ConnectionStatus::Connected).await;
        assert!(
            !store
                .sign_in_required(tenant, &account(which))
                .await
                .unwrap()
        );
    }
    control.stop(input.device_id).await.unwrap();
    assert_eq!(
        control.status().await.unwrap()[0].connection,
        ConnectionStatus::Stopped
    );
    // Pairing recovery must also enroll with the bound target's version.
    mock.host_missing.store(true, Ordering::SeqCst);
    assert!(matches!(
        control
            .configure(Command::PairIn {
                input: input.device_id
            })
            .await
            .unwrap(),
        Reply::Pairing { .. }
    ));
    assert_eq!(mock.host_generation.load(Ordering::SeqCst), 6);
    // A missing controller registration has a different repair from an account login.
    sqlx::query("DELETE FROM secrets WHERE tenant_id=? AND scope=?")
        .bind(tenant.to_string())
        .bind(crate::vault::scope_key(&SecretScope::InRegistration(input.device_id)).unwrap())
        .execute(&store.pool)
        .await
        .unwrap();
    assert!(control.start(input.device_id).await.is_err());
    let failed = control.status().await.unwrap().remove(0);
    assert_eq!(
        failed.connection,
        ConnectionStatus::ControllerRegistrationRequired
    );
    assert_eq!(failed.side, Some(FaultSide::Controller));
    mock.deny_host_enroll.store(403, Ordering::SeqCst);
    assert!(
        control
            .configure(Command::PairIn {
                input: input.device_id
            })
            .await
            .is_err()
    );
    assert_eq!(
        control.status().await.unwrap()[0].connection,
        ConnectionStatus::ControllerRegistrationRequired
    );
    mock.deny_host_enroll.store(0, Ordering::SeqCst);
    control
        .configure(Command::PairIn {
            input: input.device_id,
        })
        .await
        .unwrap();
    wait_connection(&control, ConnectionStatus::Connected).await;
    // Ordinary enroll denials retain their true side and are not a missing registration.
    for status in [401, 403] {
        mock.deny_host_enroll.store(status, Ordering::SeqCst);
        mock.known_host_404.store(true, Ordering::SeqCst);
        mock.in_reject_once.store(404, Ordering::SeqCst);
        mock.drop_in.notify_one();
        wait_connection(&control, ConnectionStatus::AccessDenied).await;
        assert_eq!(
            control.status().await.unwrap()[0].side,
            Some(FaultSide::Controller)
        );
        mock.deny_host_enroll.store(0, Ordering::SeqCst);
        control.start(input.device_id).await.unwrap();
        wait_connection(&control, ConnectionStatus::Connected).await;
    }
    stop.cancel();
    timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    mock.stop.cancel();
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[derive(Clone, Copy)]
enum RecoveryCase {
    Checkpointed,
    CheckpointWriteFailure,
    MissingRecord { deleting: bool },
}
async fn recovery_scenario(case: RecoveryCase) {
    let checkpoint_failure = matches!(case, RecoveryCase::CheckpointWriteFailure);
    let path = std::env::temp_dir().join(format!("bridge-recovery-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let mock = Arc::new(Mock::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = Upstream::local_test(&format!("http://{}", listener.local_addr().unwrap()));
    let app = axum::Router::new()
        .fallback(any(handler))
        .with_state(mock.clone());
    let shutdown = mock.stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
    let tenant = Uuid::new_v4();
    store.create_tenant(tenant).await.unwrap();
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
    let auth = Arc::new(AuthService::new(vault.clone(), upstream.clone()));
    let stop = CancellationToken::new();
    let (control, running) = crate::chains::ChainCoordinator::spawn(
        store.clone(),
        crate::chains::RuntimeFactory {
            store: store.clone(),
            vault: vault.clone(),
            auth,
            upstream: upstream.clone(),
        },
        tenant,
        stop.clone(),
    );
    complete(
        &control,
        Command::BeginAccountLogin {
            port: 1455,
            expected: Some(account("b")),
        },
        "b",
    )
    .await;
    let Reply::Endpoint(output) = control
        .configure(Command::CreateOut {
            account: account("b"),
            name: "recoverable".into(),
        })
        .await
        .unwrap()
    else {
        panic!("output")
    };
    let seed = vault.tenant_seed(tenant).await.unwrap();
    let key = identity::signing_key(&seed, tenant, &output.account, &output.identity_name).unwrap();
    *mock.public.lock().unwrap() = key.public_key().to_vec();
    *mock.identity_hash.lock().unwrap() = device::identity_hash(&key);
    let (transaction, callback) = callback(
        &control,
        Command::BeginOutLogin {
            port: 1455,
            output: output.device_id,
        },
        "step-b",
    )
    .await;
    if checkpoint_failure {
        sqlx::raw_sql("CREATE TRIGGER fail_checkpoint BEFORE INSERT ON secrets WHEN instr(NEW.scope, 'out_enrollment') > 0 BEGIN SELECT RAISE(ABORT, 'synthetic-checkpoint-failure'); END;").execute(&store.pool).await.unwrap();
    }
    mock.lose_enroll_finish.store(true, Ordering::SeqCst);
    assert!(
        control
            .configure(Command::CompleteLogin {
                transaction,
                callback
            })
            .await
            .is_err()
    );
    assert!(
        vault
            .read(tenant, &SecretScope::OutRegistration(output.device_id))
            .await
            .unwrap()
            .is_none()
    );
    if checkpoint_failure {
        assert_eq!(mock.enroll_starts.load(Ordering::SeqCst), 1);
        assert_eq!(mock.enroll_finishes.load(Ordering::SeqCst), 0);
        assert!(
            vault
                .read(tenant, &SecretScope::OutEnrollment(output.device_id))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            control
                .configure(Command::RecoverOut {
                    output: output.device_id
                })
                .await
                .is_err()
        );
        stop.cancel();
        running.await.unwrap().unwrap();
        mock.stop.cancel();
        server.await.unwrap();
        store.pool.close().await;
        std::fs::remove_file(path).unwrap();
        return;
    }
    assert!(
        vault
            .read(tenant, &SecretScope::OutEnrollment(output.device_id))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        control
            .configure(Command::BeginOutLogin {
                port: 1455,
                output: output.device_id
            })
            .await
            .is_err()
    );
    assert_eq!(mock.enroll_starts.load(Ordering::SeqCst), 1);
    assert_eq!(mock.enroll_finishes.load(Ordering::SeqCst), 1);
    if let RecoveryCase::MissingRecord { deleting } = case {
        let restore = |client_id: &str| Command::RestoreOutRecord {
            output: output.device_id,
            client_id: client_id.into(),
        };
        // Existing evidence cannot be replaced, including by a second user-supplied ID.
        assert!(control.configure(restore("controller-b")).await.is_err());
        assert_eq!(mock.refreshes.load(Ordering::SeqCst), 0);
        sqlx::query("DELETE FROM secrets WHERE tenant_id=? AND scope=?")
            .bind(tenant.to_string())
            .bind(crate::vault::scope_key(&SecretScope::OutEnrollment(output.device_id)).unwrap())
            .execute(&store.pool)
            .await
            .unwrap();
        if deleting {
            assert_eq!(
                control.delete(output.device_id).await.unwrap(),
                crate::lifecycle::DeleteOutcome::RegistrationUnknown
            );
        }
        assert!(control.configure(restore("")).await.is_err());
        assert_eq!(mock.refreshes.load(Ordering::SeqCst), 0);
        assert!(control.configure(restore("wrong-client")).await.is_err());
        *mock.identity_hash.lock().unwrap() = "foreign-device-hash".into();
        assert!(control.configure(restore("controller-b")).await.is_err());
        *mock.identity_hash.lock().unwrap() = device::identity_hash(&key);
        mock.foreign_refresh_owner.store(true, Ordering::SeqCst);
        assert!(control.configure(restore("controller-b")).await.is_err());
        mock.foreign_refresh_owner.store(false, Ordering::SeqCst);
        assert!(
            vault
                .read(tenant, &SecretScope::OutEnrollment(output.device_id))
                .await
                .unwrap()
                .is_none()
        );
        // Another OUT under the same account still has a different derived key.
        let other = identity::endpoint(
            &seed,
            tenant,
            account("b"),
            "other-output",
            EndpointKind::Out,
        )
        .unwrap();
        store.create_endpoint(&other).await.unwrap();
        store
            .begin_enrollment(tenant, other.device_id)
            .await
            .unwrap();
        assert!(
            control
                .configure(Command::RestoreOutRecord {
                    output: other.device_id,
                    client_id: "controller-b".into()
                })
                .await
                .is_err()
        );
        assert!(
            vault
                .read(tenant, &SecretScope::OutEnrollment(other.device_id))
                .await
                .unwrap()
                .is_none()
        );
        if !deleting {
            let input =
                identity::endpoint(&seed, tenant, account("b"), "bound-input", EndpointKind::In)
                    .unwrap();
            store.create_endpoint(&input).await.unwrap();
            store
                .bind(&Binding {
                    tenant_id: tenant,
                    in_id: input.device_id,
                    out_id: output.device_id,
                })
                .await
                .unwrap();
            let calls = mock.refreshes.load(Ordering::SeqCst);
            assert!(control.configure(restore("controller-b")).await.is_err());
            assert_eq!(mock.refreshes.load(Ordering::SeqCst), calls);
            store.unbind(tenant, input.device_id).await.unwrap();
        }
        sqlx::raw_sql("CREATE TRIGGER fail_restore BEFORE INSERT ON secrets WHEN instr(NEW.scope, 'out_enrollment') > 0 BEGIN SELECT RAISE(ABORT, 'synthetic-restore-failure'); END;").execute(&store.pool).await.unwrap();
        assert!(control.configure(restore("controller-b")).await.is_err());
        assert!(
            vault
                .read(tenant, &SecretScope::OutEnrollment(output.device_id))
                .await
                .unwrap()
                .is_none()
        );
        sqlx::query("DROP TRIGGER fail_restore")
            .execute(&store.pool)
            .await
            .unwrap();
        control.configure(restore("controller-b")).await.unwrap();
        assert!(
            vault
                .read(tenant, &SecretScope::OutEnrollment(output.device_id))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            vault
                .read(tenant, &SecretScope::OutRegistration(output.device_id))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .endpoint_deleting(tenant, output.device_id)
                .await
                .unwrap(),
            deleting
        );
        assert!(
            mock.sessions.lock().unwrap().is_empty(),
            "repair must not send refresh/finish"
        );
        assert_eq!(mock.revokes.load(Ordering::SeqCst), 0);
        let calls = mock.refreshes.load(Ordering::SeqCst);
        assert!(control.configure(restore("wrong-client")).await.is_err());
        assert_eq!(mock.refreshes.load(Ordering::SeqCst), calls);
    }
    stop.cancel();
    running.await.unwrap().unwrap();
    store.pool.close().await;
    // Reopen the actual database and instantiate a fresh auth/coordinator, as after process exit.
    let store = Arc::new(SqliteStore::open(options).await.unwrap());
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
    let auth = Arc::new(AuthService::new(vault.clone(), upstream.clone()));
    let stop = CancellationToken::new();
    let (control, running) = crate::chains::ChainCoordinator::spawn(
        store.clone(),
        crate::chains::RuntimeFactory {
            store: store.clone(),
            vault: vault.clone(),
            auth,
            upstream,
        },
        tenant,
        stop.clone(),
    );
    if matches!(case, RecoveryCase::MissingRecord { deleting: true }) {
        // Startup resumes only the scoped revoke, never registration recovery or activation.
        assert!(control.deleting().await.unwrap().is_empty());
        assert!(
            store
                .endpoint(tenant, output.device_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(mock.revokes.load(Ordering::SeqCst), 1);
        assert!(mock.sessions.lock().unwrap().is_empty());
        assert!(
            vault
                .read(tenant, &SecretScope::Account(account("b")))
                .await
                .unwrap()
                .is_some()
        );
        store.create_endpoint(&output).await.unwrap();
        assert_eq!(mock.enroll_starts.load(Ordering::SeqCst), 1);
        stop.cancel();
        running.await.unwrap().unwrap();
        mock.stop.cancel();
        server.await.unwrap();
        store.pool.close().await;
        std::fs::remove_file(path).unwrap();
        return;
    }
    *mock.identity_hash.lock().unwrap() = "foreign-device-hash".into();
    assert!(
        control
            .configure(Command::RecoverOut {
                output: output.device_id
            })
            .await
            .is_err()
    );
    assert!(
        vault
            .read(tenant, &SecretScope::OutRegistration(output.device_id))
            .await
            .unwrap()
            .is_none()
    );
    *mock.identity_hash.lock().unwrap() = device::identity_hash(&key);
    control
        .configure(Command::RecoverOut {
            output: output.device_id,
        })
        .await
        .unwrap();
    let registration: Registration = load_registration(
        &vault,
        tenant,
        SecretScope::OutRegistration(output.device_id),
    )
    .await
    .unwrap();
    assert_eq!(registration.client_id, "controller-b");
    assert_eq!(registration.key_id, device::identity(&key).key_id);
    assert_eq!(mock.enroll_starts.load(Ordering::SeqCst), 1);
    assert!(
        vault
            .read(tenant, &SecretScope::OutEnrollment(output.device_id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        control
            .configure(Command::RecoverOut {
                output: output.device_id
            })
            .await
            .is_err()
    );
    stop.cancel();
    running.await.unwrap().unwrap();
    mock.stop.cancel();
    server.await.unwrap();
    store.pool.close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn lost_enroll_finish_recovers_from_disk_without_new_identity_or_reenrollment() {
    recovery_scenario(RecoveryCase::Checkpointed).await;
}
#[tokio::test]
async fn checkpoint_write_failure_never_sends_enrollment_finish() {
    recovery_scenario(RecoveryCase::CheckpointWriteFailure).await;
}
#[tokio::test]
async fn missing_out_record_requires_original_identity_then_survives_restart_and_recovers() {
    recovery_scenario(RecoveryCase::MissingRecord { deleting: false }).await;
}
#[tokio::test]
async fn missing_deleting_out_record_only_restores_scoped_cleanup_after_restart() {
    recovery_scenario(RecoveryCase::MissingRecord { deleting: true }).await;
}

async fn wait_connection(control: &crate::chains::ChainController, expected: ConnectionStatus) {
    let mut observed = ConnectionStatus::Stopped;
    let result = timeout(Duration::from_secs(10), async {
        loop {
            observed = control.status().await.unwrap()[0].connection;
            if observed == expected {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "expected {expected:?}, observed {observed:?}"
    );
}

#[tokio::test]
async fn invalid_step_up_leaves_out_retryable_and_failed_proof_keeps_checkpoint() {
    let mock = Arc::new(Mock::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = Upstream::local_test(&format!("http://{}", listener.local_addr().unwrap()));
    let app = axum::Router::new()
        .fallback(any(handler))
        .with_state(mock.clone());
    let shutdown = mock.stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
    let tenant = Uuid::new_v4();
    vault.create_tenant_with_seed(tenant).await.unwrap();
    let auth = Arc::new(AuthService::new(vault.clone(), upstream.clone()));
    let stop = CancellationToken::new();
    let (control, running) = crate::chains::ChainCoordinator::spawn(
        store.clone(),
        crate::chains::RuntimeFactory {
            store: store.clone(),
            vault: vault.clone(),
            auth,
            upstream,
        },
        tenant,
        stop.clone(),
    );
    complete(
        &control,
        Command::BeginAccountLogin {
            port: 1455,
            expected: Some(account("b")),
        },
        "b",
    )
    .await;
    let Reply::Endpoint(output) = control
        .configure(Command::CreateOut {
            account: account("b"),
            name: "retryable".into(),
        })
        .await
        .unwrap()
    else {
        panic!("output")
    };
    let seed = vault.tenant_seed(tenant).await.unwrap();
    let key = identity::signing_key(&seed, tenant, &output.account, &output.identity_name).unwrap();
    *mock.public.lock().unwrap() = key.public_key().to_vec();
    *mock.identity_hash.lock().unwrap() = device::identity_hash(&key);
    for code in [
        "step-stale-b",
        "step-wrong-b",
        "step-scope-b",
        "step-expired-b",
    ] {
        let (transaction, callback) = callback(
            &control,
            Command::BeginOutLogin {
                port: 1455,
                output: output.device_id,
            },
            code,
        )
        .await;
        assert!(
            control
                .configure(Command::CompleteLogin {
                    transaction,
                    callback
                })
                .await
                .is_err()
        );
        assert!(!store.sign_in_required(tenant, &account("b")).await.unwrap());
        assert_eq!(mock.enroll_starts.load(Ordering::SeqCst), 0);
        let attempts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM enrollment_attempts WHERE tenant_id=? AND device_id=?",
        )
        .bind(tenant.to_string())
        .bind(output.device_id.to_string())
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(attempts, 0, "local rejection must allow retry or deletion");
    }
    // The very same OUT can still complete an ordinary, valid authorization.
    complete(
        &control,
        Command::BeginOutLogin {
            port: 1455,
            output: output.device_id,
        },
        "step-b",
    )
    .await;
    assert_eq!(mock.enroll_finishes.load(Ordering::SeqCst), 1);
    assert!(
        vault
            .read(tenant, &SecretScope::OutRegistration(output.device_id))
            .await
            .unwrap()
            .is_some()
    );

    // After start succeeds, even a challenge that expires before proof creation must leave recovery evidence.
    let Reply::Endpoint(other) = control
        .configure(Command::CreateOut {
            account: account("b"),
            name: "proof-failure".into(),
        })
        .await
        .unwrap()
    else {
        panic!("output")
    };
    let key = identity::signing_key(&seed, tenant, &other.account, &other.identity_name).unwrap();
    *mock.public.lock().unwrap() = key.public_key().to_vec();
    *mock.identity_hash.lock().unwrap() = device::identity_hash(&key);
    mock.expired_enroll_challenge.store(true, Ordering::SeqCst);
    let (transaction, callback) = callback(
        &control,
        Command::BeginOutLogin {
            port: 1455,
            output: other.device_id,
        },
        "step-b",
    )
    .await;
    assert!(
        control
            .configure(Command::CompleteLogin {
                transaction,
                callback
            })
            .await
            .is_err()
    );
    assert_eq!(mock.enroll_starts.load(Ordering::SeqCst), 2);
    assert_eq!(mock.enroll_finishes.load(Ordering::SeqCst), 1);
    assert!(
        vault
            .read(tenant, &SecretScope::OutEnrollment(other.device_id))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        vault
            .read(tenant, &SecretScope::OutRegistration(other.device_id))
            .await
            .unwrap()
            .is_none()
    );
    // Existing scoped cleanup uses that original checkpoint; no manual client ID repair is needed.
    control.delete(other.device_id).await.unwrap();
    assert_eq!(mock.revokes.load(Ordering::SeqCst), 1);
    assert!(
        store
            .endpoint(tenant, other.device_id)
            .await
            .unwrap()
            .is_none()
    );
    stop.cancel();
    running.await.unwrap().unwrap();
    mock.stop.cancel();
    server.await.unwrap();
    store.pool.close().await;
}

#[tokio::test]
async fn host_replacement_coalesces_retries_after_restart_and_finishes_before_deletion() {
    for (uncertain, deleting) in [(false, false), (true, false), (false, true)] {
        let mock = Arc::new(Mock::default());
        mock.host_generation.store(1, Ordering::SeqCst);
        // A WebSocket reported stale enrollment even though HTTP refresh still succeeds.
        mock.fail_host_enroll_once
            .store(uncertain, Ordering::SeqCst);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = Upstream::local_test(&format!("http://{}", listener.local_addr().unwrap()));
        let app = axum::Router::new()
            .fallback(any(handler))
            .with_state(mock.clone());
        let cancel = mock.stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(cancel.cancelled_owned())
                .await
                .unwrap();
        });
        let path =
            std::env::temp_dir().join(format!("bridge-host-recovery-{}.sqlite", Uuid::new_v4()));
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let mut store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
        let tenant = Uuid::new_v4();
        store.create_tenant(tenant).await.unwrap();
        store.create_account(tenant, &account("a")).await.unwrap();
        let mut vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
        let mut auth = Arc::new(AuthService::new(vault.clone(), upstream.clone()));
        let seed = vault.tenant_seed(tenant).await.unwrap();
        let input = identity::endpoint(
            &seed,
            tenant,
            account("a"),
            "host-recovery",
            EndpointKind::In,
        )
        .unwrap();
        store.create_endpoint(&input).await.unwrap();
        vault.write(tenant, &SecretScope::Account(input.account.clone()), &serde_json::to_vec(&json!({"account":input.account,"access":token("a",false),"refresh":"synthetic-refresh-a","global_user":"global-a","auth_user":null,"expires_at":unix_time()+3600})).unwrap()).await.unwrap();
        vault
            .save_host_registration(
                tenant,
                input.device_id,
                &HostRegistration {
                    server_id: "host-a".into(),
                    environment_id: "environment-a".into(),
                },
            )
            .await
            .unwrap();
        if uncertain {
            let error = upstream
                .registered_host_session(&auth, &vault, &input, "mock-codex/1", Some("host-a"))
                .await
                .err()
                .unwrap();
            assert!(matches!(error, ControlError::Rejected(503)));
            assert!(error.retryable());
            store.pool.close().await;
            store = Arc::new(SqliteStore::open(options).await.unwrap());
            vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
            auth = Arc::new(AuthService::new(vault.clone(), upstream.clone()));
        }
        if deleting {
            mock.hold_host_enroll.store(true, Ordering::SeqCst);
            let (result, ()) = tokio::join!(
                upstream.registered_host_session(
                    &auth,
                    &vault,
                    &input,
                    "mock-codex/1",
                    Some("host-a")
                ),
                async {
                    mock.host_enroll_entered.notified().await;
                    store.begin_delete(&input).await.unwrap();
                    mock.host_enroll_release.notify_one();
                }
            );
            assert_eq!(result.unwrap().server_id, "host-a-new");
            assert!(
                store
                    .endpoint_deleting(tenant, input.device_id)
                    .await
                    .unwrap()
            );
            assert!(matches!(
                upstream
                    .registered_host_session(&auth, &vault, &input, "mock-codex/1", None)
                    .await,
                Err(ControlError::Auth(crate::auth::AuthError::Identity))
            ));
        } else {
            let stale = if uncertain { None } else { Some("host-a") };
            let (a, b) = tokio::join!(
                upstream.registered_host_session(&auth, &vault, &input, "mock-codex/1", stale),
                upstream.registered_host_session(&auth, &vault, &input, "mock-codex/1", stale)
            );
            assert_eq!(a.unwrap().server_id, "host-a-new");
            assert_eq!(b.unwrap().server_id, "host-a-new");
        }
        assert_eq!(
            mock.host_generation.load(Ordering::SeqCst),
            if uncertain { 3 } else { 2 }
        );
        let saved: HostRegistration =
            load_registration(&vault, tenant, SecretScope::InRegistration(input.device_id))
                .await
                .unwrap();
        assert_eq!(saved.server_id, "host-a-new");
        assert_eq!(saved.environment_id, "environment-a-new");
        assert!(
            !store
                .enrollment_pending(tenant, input.device_id)
                .await
                .unwrap()
        );
        mock.stop.cancel();
        server.await.unwrap();
        store.pool.close().await;
        std::fs::remove_file(path).unwrap();
    }
}
