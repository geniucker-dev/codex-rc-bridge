use super::*;
use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, Uri},
};
use sqlx::sqlite::SqliteConnectOptions;
async fn service() -> Arc<DesktopService> {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    DesktopService::open(store, EncryptionKey::from_bytes(&[43; 32]).unwrap())
        .await
        .unwrap()
}
#[tokio::test]
async fn desktop_snapshot_includes_pairing_pending_for_both_endpoint_kinds() {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let service =
        DesktopService::open(store.clone(), EncryptionKey::from_bytes(&[43; 32]).unwrap())
            .await
            .unwrap();
    let account = AccountIdentity {
        workspace_id: "synthetic-workspace".into(),
        user_id: "synthetic-user".into(),
    };
    store
        .create_account(service.tenant, &account)
        .await
        .unwrap();
    let seed = service.vault.tenant_seed(service.tenant).await.unwrap();
    for kind in [EndpointKind::In, EndpointKind::Out] {
        let endpoint = bridge_core::identity::endpoint(
            &seed,
            service.tenant,
            account.clone(),
            "synthetic-device",
            kind,
        )
        .unwrap();
        store.create_endpoint(&endpoint).await.unwrap();
    }
    let snapshot = service
        .request("/state".into(), "GET".into(), Value::Null)
        .await
        .unwrap();
    assert_eq!(
        snapshot["configuration"]["value"]["accounts"][0]["sign_in_required"],
        false
    );
    let endpoints = snapshot["configuration"]["value"]["endpoints"]
        .as_array()
        .unwrap();
    assert_eq!(endpoints.len(), 2);
    for endpoint in endpoints {
        assert_eq!(endpoint["pairing_pending"], false);
        assert_eq!(endpoint["paired_device"], Value::Null);
        assert_eq!(endpoint["pairing_error"], Value::Null);
    }
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn desktop_owns_one_tenant_and_network_updates_preserve_identity() {
    let service = service().await;
    let initial = service.vault.tenant_seed(service.tenant).await.unwrap();
    let account = AccountIdentity {
        workspace_id: "workspace".into(),
        user_id: "user".into(),
    };
    let before = bridge_core::identity::endpoint(
        &initial,
        service.tenant,
        account.clone(),
        "test",
        EndpointKind::Out,
    )
    .unwrap();
    let session = service
        .request("/session".into(), "GET".into(), Value::Null)
        .await
        .unwrap();
    assert_eq!(session["identity"]["tenant_id"], service.tenant.to_string());
    assert!(
        service
            .request(
                "/accounts/login".into(),
                "POST".into(),
                json!({"tenant_id":Uuid::new_v4()})
            )
            .await
            .is_err()
    );
    let changed = service
        .request(
            "/deployment/network".into(),
            "POST".into(),
            json!({"proxy_url":"http://127.0.0.1:9"}),
        )
        .await
        .unwrap();
    assert_eq!(changed["restart_failures"], 0);
    assert_eq!(changed["mode"], "proxy");
    let seed = service.vault.tenant_seed(service.tenant).await.unwrap();
    assert_eq!(
        before.device_id,
        bridge_core::identity::endpoint(&seed, service.tenant, account, "test", EndpointKind::Out)
            .unwrap()
            .device_id
    );
    service.shutdown().await.unwrap();
    assert!(
        service
            .request("/state".into(), "GET".into(), Value::Null)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn callback_rejects_foreign_state_and_consumes_matching_transaction_once() {
    let service = service().await;
    let Reply::Login(login) = service
        .controller
        .configure(Command::BeginAccountLogin {
            port: 1455,
            expected: None,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let url = url::Url::parse(&login.url).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    service.callbacks.lock().await.insert(
        state.clone(),
        Pending {
            port: 1455,
            transaction: login.id,
            expires: login.expires_at,
        },
    );
    let uri: Uri = format!("/auth/callback?state={state}&error=access_denied")
        .parse()
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("evil.example:1455"));
    assert_eq!(
        callback(State(service.clone()), headers.clone(), uri.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(service.callbacks.lock().await.len(), 1);
    headers.insert("host", HeaderValue::from_static("localhost:1455"));
    assert_eq!(
        callback(
            State(service.clone()),
            headers.clone(),
            "/auth/callback?state=wrong&error=access_denied"
                .parse()
                .unwrap()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(service.callbacks.lock().await.len(), 1);
    assert_eq!(
        callback(State(service.clone()), headers.clone(), uri.clone())
            .await
            .0,
        StatusCode::OK
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if matches!(
                service.login_results.lock().await.get(&login.id),
                Some(LoginStatus::Finished { .. })
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(service.login_status(login.id).await["status"], "failed");
    assert_eq!(
        service.login_status(login.id).await["error"]["code"],
        "upstream_auth_declined"
    );
    assert_eq!(
        callback(State(service.clone()), headers, uri).await.0,
        StatusCode::BAD_REQUEST
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn login_expiry_does_not_erase_processing_or_completed_result() {
    let service = service().await;
    let id = Uuid::new_v4();
    assert_eq!(service.login_status(id).await["status"], "expired");
    service
        .login_results
        .lock()
        .await
        .insert(id, LoginStatus::Pending(unix_time() - 1));
    assert_eq!(service.login_status(id).await["status"], "expired");
    service
        .login_results
        .lock()
        .await
        .insert(id, LoginStatus::Pending(unix_time() + 60));
    assert_eq!(service.login_status(id).await["status"], "pending");
    // A callback accepted before its deadline can still be processing afterwards.
    service
        .login_results
        .lock()
        .await
        .insert(id, LoginStatus::Processing);
    assert_eq!(service.login_status(id).await["status"], "processing");
    service.finish_login(id, Ok(())).await;
    assert_eq!(service.login_status(id).await["status"], "complete");
    service.login_results.lock().await.insert(
        id,
        LoginStatus::Finished {
            retain_until: unix_time() - 1,
            result: Ok(()),
        },
    );
    assert_eq!(service.login_status(id).await["status"], "expired");
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn manual_callback_reports_the_same_classification_as_web() {
    let service = service().await;
    let id = Uuid::new_v4();
    let error = service.request(format!("/logins/{id}"), "POST".into(),
        json!({"callback":"http://localhost:1455/auth/callback?code=synthetic&state=synthetic"}))
        .await.err().unwrap();
    assert_eq!(error.code, Some("upstream_auth_transaction"));
    assert_eq!(
        service.login_status(id).await["error"]["code"],
        "upstream_auth_transaction"
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn review_finished_callback_result_survives_transaction_expiry() {
    let service = service().await;
    let auth = AuthService::new(service.vault.clone(), service.upstream.clone());
    let login = auth
        .begin(
            service.tenant,
            bridge_core::auth::LoginPurpose::Account { expected: None },
            1455,
            unix_time() - 598,
        )
        .await
        .unwrap();
    let url = url::Url::parse(&login.url).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    service.callbacks.lock().await.insert(
        state.clone(),
        Pending {
            port: 1455,
            transaction: login.id,
            expires: login.expires_at,
        },
    );
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("localhost:1455"));
    assert_eq!(
        callback(
            State(service.clone()),
            headers,
            format!("/auth/callback?state={state}&error=access_denied")
                .parse()
                .unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    while !matches!(
        service.login_results.lock().await.get(&login.id),
        Some(LoginStatus::Finished { .. })
    ) {
        tokio::task::yield_now().await;
    }
    while unix_time() <= login.expires_at {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let status = service
        .request(format!("/logins/{}", login.id), "GET".into(), Value::Null)
        .await
        .unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(
        status["status"], "failed",
        "completed result must not revert to pending after original OAuth TTL"
    );
}
#[tokio::test]
async fn review_first_seed_write_failure_can_retry() {
    let path = std::env::temp_dir().join(format!("bridge-review-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
    let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_seed BEFORE INSERT ON secrets BEGIN SELECT RAISE(ABORT, 'injected write failure'); END").execute(&pool).await.unwrap();
    let first =
        DesktopService::open(store.clone(), EncryptionKey::from_bytes(&[43; 32]).unwrap()).await;
    assert!(first.is_err());
    sqlx::query("DROP TRIGGER fail_seed")
        .execute(&pool)
        .await
        .unwrap();
    let retry =
        DesktopService::open(store.clone(), EncryptionKey::from_bytes(&[43; 32]).unwrap()).await;
    let error = match retry {
        Ok(service) => {
            service.shutdown().await.unwrap();
            None
        }
        Err(error) => Some(error.message),
    };
    pool.close().await;
    drop(store);
    std::fs::remove_file(path).unwrap();
    assert!(
        error.is_none(),
        "first-launch seed failure permanently bricks startup: {error:?}"
    );
}

#[tokio::test]
async fn callback_listener_falls_back_and_reuses_the_selected_port() {
    let first = service().await;
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let spare = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let busy = occupied.local_addr().unwrap().port();
    let free = spare.local_addr().unwrap().port();
    drop(spare);
    let mut task = first.callback_task.lock().await;
    assert_eq!(
        first.start_callback(&mut task, &[busy, free]).await,
        Some(free)
    );
    assert_eq!(first.start_callback(&mut task, &[busy]).await, Some(free));
    drop(task);
    first.shutdown().await.unwrap();
    let other = service().await;
    let mut task = other.callback_task.lock().await;
    assert_eq!(other.start_callback(&mut task, &[busy]).await, None);
    *task = Some((free, tokio::spawn(async {})));
    while !task.as_ref().unwrap().1.is_finished() {
        tokio::task::yield_now().await;
    }
    assert_eq!(other.start_callback(&mut task, &[busy]).await, None);
    assert!(task.is_none());
    drop(task);
    other.shutdown().await.unwrap();
}

#[tokio::test]
async fn callback_on_fallback_port_matches_its_authorization_transaction() {
    let service = service().await;
    let Reply::Login(login) = service
        .controller
        .configure(Command::BeginAccountLogin {
            expected: None,
            port: 1457,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let url = url::Url::parse(&login.url).unwrap();
    assert_eq!(
        url.query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .unwrap()
            .1,
        "http://localhost:1457/auth/callback"
    );
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    service.callbacks.lock().await.insert(
        state.clone(),
        Pending {
            port: 1457,
            transaction: login.id,
            expires: login.expires_at,
        },
    );
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("localhost:1455"));
    assert_eq!(
        callback(
            State(service.clone()),
            headers.clone(),
            format!("/auth/callback?state={state}&error=access_denied")
                .parse()
                .unwrap()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(service.callbacks.lock().await.contains_key(&state));
    headers.insert("host", HeaderValue::from_static("localhost:1457"));
    assert_eq!(
        callback(
            State(service.clone()),
            headers,
            format!("/auth/callback?state={state}&error=access_denied")
                .parse()
                .unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    loop {
        if matches!(
            service.login_results.lock().await.get(&login.id),
            Some(LoginStatus::Finished { .. })
        ) {
            break;
        }
        tokio::task::yield_now().await;
    }
    service.shutdown().await.unwrap();
}

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
        command: Command,
    ) -> Result<Reply, bridge_core::configuration::ConfigurationError> {
        use bridge_core::output_pairing::{PairOutputError, PairedOutput};
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
async fn desktop_pair_contract_matches_http_and_rejects_retired_target_paths() {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let service =
        DesktopService::open(store.clone(), EncryptionKey::from_bytes(&[43; 32]).unwrap())
            .await
            .unwrap();
    service.shutdown().await.unwrap();
    let mut service = Arc::try_unwrap(service).ok().unwrap();
    let account = AccountIdentity {
        workspace_id: "synthetic".into(),
        user_id: "user".into(),
    };
    store
        .create_account(service.tenant, &account)
        .await
        .unwrap();
    let endpoint = bridge_core::identity::endpoint(
        &service.vault.tenant_seed(service.tenant).await.unwrap(),
        service.tenant,
        account,
        "desk",
        EndpointKind::Out,
    )
    .unwrap();
    store.create_endpoint(&endpoint).await.unwrap();
    let stop = CancellationToken::new();
    let (controller, worker) =
        ChainCoordinator::spawn(store, PairContract, service.tenant, stop.clone());
    service.controller = controller;
    service.stop = stop;
    service.worker = Mutex::new(Some(worker));
    let service = Arc::new(service);
    let value = service
        .request(
            format!("/inputs/{}/device/sync", Uuid::new_v4()),
            "POST".into(),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(value, Value::Null);
    let error = service
        .request(
            format!("/inputs/{}/device/sync", Uuid::nil()),
            "POST".into(),
            Value::Null,
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, 409);
    assert_eq!(
        error.code,
        Some("operation_failed_check_authorization_or_state")
    );
    let value = service
        .request(
            format!("/outputs/{}/device/sync", Uuid::new_v4()),
            "POST".into(),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(value, Value::Null);
    let error = service
        .request(
            format!("/outputs/{}/device/sync", Uuid::nil()),
            "POST".into(),
            Value::Null,
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, 409);
    assert_eq!(
        error.code,
        Some("operation_failed_check_authorization_or_state")
    );
    for (user, expected) in [
        ("account", "account_sign_in_required"),
        ("step-up", "device_authorization_required"),
    ] {
        let error = service
            .request(
                "/accounts/check".into(),
                "POST".into(),
                json!({"account":{"workspace_id":"synthetic","user_id":user}}),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.status, 409);
        assert_eq!(error.code, Some(expected));
    }
    let error = service
        .request(
            format!("/inputs/{}/pair", Uuid::nil()),
            "POST".into(),
            Value::Null,
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, 409);
    assert_eq!(error.code, Some("in_pairing_cleanup_pending"));
    let result = service
        .request(
            format!("/inputs/{}/pair", Uuid::new_v4()),
            "POST".into(),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({"result":"pairing","value":{"code":"SYNTHETIC-CODE","expires_at":1234}})
    );
    for (method, suffix) in [("GET", "clients"), ("POST", "clients/revoke")] {
        assert!(
            service
                .request(
                    format!("/inputs/{}/{suffix}", Uuid::new_v4()),
                    method.into(),
                    json!({"client_id":"old-device"})
                )
                .await
                .is_err()
        );
    }
    let path = format!("/outputs/{}/pair", endpoint.device_id);
    let result = service
        .request(path.clone(), "POST".into(), json!({"code":"ABCD-EFGH"}))
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({"result":"paired_output","value":{"environment_id":"synthetic-device","name":null}})
    );
    for (code, expected) in [
        ("NONE-NONE", "out_pairing_no_environment"),
        ("MANY-MANY", "out_pairing_multiple_environments"),
        ("WAIT-WAIT", "out_pairing_cleanup_pending"),
    ] {
        let error = service
            .request(path.clone(), "POST".into(), json!({"code":code}))
            .await
            .unwrap_err();
        assert_eq!(error.status, 409);
        assert_eq!(error.code, Some(expected));
    }
    assert!(
        service
            .request(
                path,
                "POST".into(),
                json!({"code":"ABCD-EFGH","tenant_id":Uuid::new_v4()})
            )
            .await
            .is_err()
    );
    for (method, suffix) in [("GET", "targets"), ("POST", "target")] {
        assert!(
            service
                .request(
                    format!("/outputs/{}/{suffix}", endpoint.device_id),
                    method.into(),
                    json!({"environment":"anything"})
                )
                .await
                .is_err()
        );
    }
    service.shutdown().await.unwrap();
}
