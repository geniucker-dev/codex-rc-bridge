#![cfg(unix)]
use bridge_core::{
    auth::{AuthError, AuthService, LoginPurpose, SecretString},
    identity,
    management::ManagementAuth,
    model::{AccountIdentity, Binding, EndpointKind},
    storage::SqliteStore,
    upstream::Upstream,
    vault::Vault,
};
use bridge_server::key_file;
use sqlx::sqlite::SqliteConnectOptions;
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;
struct Fixture {
    path: PathBuf,
    child: Option<Child>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
#[tokio::test]
async fn configured_server_restores_offline_database_and_separate_key_and_joins_on_sigterm() {
    let mut fixture = Fixture {
        path: std::env::temp_dir().join(format!("bridge-server-test-{}", Uuid::new_v4())),
        child: None,
    };
    std::fs::create_dir_all(fixture.path.join("data")).unwrap();
    std::fs::create_dir_all(fixture.path.join("keys")).unwrap();
    let mut db = fixture.path.join("data/state.sqlite");
    let mut key = fixture.path.join("keys/master.key");
    key_file::create(&key).unwrap();
    let store = Arc::new(
        SqliteStore::open(
            SqliteConnectOptions::new()
                .filename(&db)
                .create_if_missing(true),
        )
        .await
        .unwrap(),
    );
    let tenant = Uuid::new_v4();
    store.create_tenant(tenant).await.unwrap();
    let vault = Vault::new(store.clone(), key_file::load(&key).unwrap());
    let seed = vault.tenant_seed(tenant).await.unwrap();
    let account = AccountIdentity {
        workspace_id: "synthetic-workspace".into(),
        user_id: "synthetic-user".into(),
    };
    store.create_account(tenant, &account).await.unwrap();
    let input =
        identity::endpoint(&seed, tenant, account.clone(), "phone", EndpointKind::In).unwrap();
    let output =
        identity::endpoint(&seed, tenant, account.clone(), "desk", EndpointKind::Out).unwrap();
    let public_key = identity::signing_key(&seed, tenant, &account, "desk")
        .unwrap()
        .public_key()
        .to_vec();
    for endpoint in [&input, &output] {
        store.create_endpoint(endpoint).await.unwrap();
    }
    store
        .bind(&Binding {
            tenant_id: tenant,
            in_id: input.device_id,
            out_id: output.device_id,
        })
        .await
        .unwrap();
    // Keep the synthetic chain disabled: this exercise must never contact upstream services.
    assert!(
        store
            .claim_stream(tenant, input.device_id, "synthetic-retired-stream")
            .await
            .unwrap()
    );
    let auth = AuthService::new(vault.clone(), Upstream::new(None).unwrap());
    let pending = auth
        .begin(
            tenant,
            LoginPurpose::Account {
                expected: Some(account.clone()),
            },
            1455,
            bridge_core::upstream::unix_time(),
        )
        .await
        .unwrap();
    let login_url = url::Url::parse(&pending.url).unwrap();
    let state = login_url
        .query_pairs()
        .find(|(name, _)| name == "state")
        .unwrap()
        .1
        .into_owned();
    let mut declined_callback = url::Url::parse("http://localhost:1455/auth/callback").unwrap();
    declined_callback
        .query_pairs_mut()
        .append_pair("state", &state)
        .append_pair("error", "access_denied");
    drop(auth);
    ManagementAuth::new(store.clone())
        .create_user(
            tenant,
            "demo",
            SecretString::new("synthetic-password-only".into()),
        )
        .await
        .unwrap();
    drop(vault);
    Arc::try_unwrap(store).ok().unwrap().close().await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let mut original_cookie = None;
    for restored in [false, true] {
        let address = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let origin = format!("http://{address}");
        fixture.child = Some(
            Command::new(env!("CARGO_BIN_EXE_bridge-server"))
                .env("BRIDGE_DATABASE", &db)
                .env("BRIDGE_MASTER_KEY", &key)
                .env("BRIDGE_BIND", address.to_string())
                .env("BRIDGE_PUBLIC_ORIGIN", &origin)
                .env_remove("BRIDGE_UPSTREAM_PROXY")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut ready = false;
        for _ in 0..100 {
            if client
                .get(format!("{origin}/api/capabilities"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                ready = true;
                break;
            }
            assert!(
                fixture
                    .child
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap()
                    .is_none(),
                "test server exited before readiness"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(ready, "test server did not start");
        assert!(key_file::lock_database(&db).is_err());
        let login = client
            .post(format!("{origin}/api/session"))
            .header("origin", &origin)
            .json(&serde_json::json!({"username":"demo","password":"synthetic-password-only"}))
            .send()
            .await
            .unwrap();
        assert_eq!(login.status(), 200);
        let cookie = login.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let state = client
            .get(format!("{origin}/api/state"))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(state.status(), 200);
        let snapshot = state.json::<serde_json::Value>().await.unwrap();
        assert_eq!(snapshot["chains"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["chains"][0]["enabled"], false);
        assert_eq!(snapshot["chains"][0]["status"], "stopped");
        let endpoints = snapshot["configuration"]["value"]["endpoints"]
            .as_array()
            .unwrap();
        assert_eq!(endpoints.len(), 2);
        assert!(
            endpoints
                .iter()
                .any(|e| e["endpoint"]["device_id"] == input.device_id.to_string())
        );
        assert!(
            endpoints
                .iter()
                .any(|e| e["endpoint"]["device_id"] == output.device_id.to_string())
        );
        if let Some(previous) = &original_cookie {
            assert_eq!(
                client
                    .get(format!("{origin}/api/state"))
                    .header("cookie", previous)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                200
            );
        } else {
            original_cookie = Some(cookie.clone());
        }
        // A real streaming response must not prevent graceful process shutdown.
        let mut events = client
            .get(format!("{origin}/api/events"))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(events.status(), 200);
        assert!(
            String::from_utf8(events.chunk().await.unwrap().unwrap().to_vec())
                .unwrap()
                .contains("event: changed")
        );
        // SAFETY: the pid identifies the child just spawned by this test, not an existing user process.
        assert_eq!(
            unsafe { libc::kill(fixture.child.as_ref().unwrap().id() as i32, libc::SIGTERM) },
            0
        );
        let status = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(status) = fixture.child.as_mut().unwrap().try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(status.success());
        assert!(events.chunk().await.unwrap().is_none());
        fixture.child = None;
        assert!(key_file::lock_database(&db).is_ok());
        if !restored {
            // Stop, copy both artifacts to separate private directories, then remove the source.
            // No logs, account state or keys outside this test's temporary directory are touched.
            use std::os::unix::fs::PermissionsExt;
            let backup = fixture.path.join("restore");
            for directory in [backup.join("data"), backup.join("keys")] {
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            std::fs::copy(&db, backup.join("data/state.sqlite")).unwrap();
            std::fs::copy(&key, backup.join("keys/master.key")).unwrap();
            std::fs::remove_dir_all(fixture.path.join("data")).unwrap();
            std::fs::remove_dir_all(fixture.path.join("keys")).unwrap();
            db = backup.join("data/state.sqlite");
            key = backup.join("keys/master.key");
            assert_eq!(
                std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let store = Arc::new(
                SqliteStore::open(SqliteConnectOptions::new().filename(&db))
                    .await
                    .unwrap(),
            );
            let vault = Vault::new(store.clone(), key_file::load(&key).unwrap());
            let seed = vault.tenant_seed(tenant).await.unwrap();
            assert_eq!(
                identity::endpoint(&seed, tenant, account.clone(), "desk", EndpointKind::Out)
                    .unwrap(),
                output
            );
            assert_eq!(
                identity::signing_key(&seed, tenant, &account, "desk")
                    .unwrap()
                    .public_key(),
                public_key
            );
            assert_eq!(
                store
                    .binding(tenant, input.device_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .out_id,
                output.device_id
            );
            assert!(
                !store
                    .claim_stream(tenant, input.device_id, "synthetic-retired-stream")
                    .await
                    .unwrap()
            );
            let auth = AuthService::new(vault.clone(), Upstream::new(None).unwrap());
            // A matching declined callback decrypts and consumes the recovered transaction,
            // without exchanging a code or making any network request.
            assert!(matches!(
                auth.complete(
                    tenant,
                    pending.id,
                    declined_callback.as_str(),
                    bridge_core::upstream::unix_time()
                )
                .await,
                Err(AuthError::Declined)
            ));
            assert!(matches!(
                auth.complete(
                    tenant,
                    pending.id,
                    declined_callback.as_str(),
                    bridge_core::upstream::unix_time()
                )
                .await,
                Err(AuthError::Transaction)
            ));
            drop(auth);
            drop(vault);
            Arc::try_unwrap(store).ok().unwrap().close().await;
            // A missing or unrelated key must fail startup, never generate replacement identity.
            let wrong_key = backup.join("keys/wrong.key");
            key_file::create(&wrong_key).unwrap();
            for invalid in [backup.join("keys/missing.key"), wrong_key] {
                fixture.child = Some(
                    Command::new(env!("CARGO_BIN_EXE_bridge-server"))
                        .env("BRIDGE_DATABASE", &db)
                        .env("BRIDGE_MASTER_KEY", &invalid)
                        .env("BRIDGE_BIND", "127.0.0.1:0")
                        .env("BRIDGE_PUBLIC_ORIGIN", "http://localhost")
                        .env_remove("BRIDGE_UPSTREAM_PROXY")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .unwrap(),
                );
                let status = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let Some(status) = fixture.child.as_mut().unwrap().try_wait().unwrap() {
                            break status;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
                assert!(!status.success());
                fixture.child = None;
            }
            assert!(!backup.join("keys/missing.key").exists());
        }
    }
}
