#![cfg(unix)]
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};
use uuid::Uuid;

struct State(PathBuf);
impl State {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("bridge-admin-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join("keys")).unwrap();
        Self(root)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_bridge-admin"))
            .arg(self.0.join("data/state.sqlite"))
            .arg(self.0.join("keys/master.key"))
            .args(args)
            .env_remove("BRIDGE_UPSTREAM_PROXY")
            .output()
            .unwrap()
    }
}

#[test]
fn local_console_handles_commands_and_eof_without_network_or_lingering_workers() {
    let state = State::new();
    let initialized = state.run(&["init"]);
    assert!(initialized.status.success());
    let stdout = String::from_utf8(initialized.stdout).unwrap();
    let tenant = stdout.split("Tenant：").nth(1).unwrap().trim();
    let mut child = Command::new(env!("CARGO_BIN_EXE_bridge-admin"))
        .arg(state.0.join("data/state.sqlite"))
        .arg(state.0.join("keys/master.key"))
        .args(["manage", tenant])
        .env_remove("BRIDGE_UPSTREAM_PROXY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"list\ninvalid-command\nlist\n").unwrap();
    drop(input); // EOF follows the same joined shutdown path as quit.
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("操作失败")
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("等待凭据操作完成")
    );
    // The process lock is released and the state remains usable.
    assert!(state.run(&["login", tenant]).status.success());
}
impl Drop for State {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn failed_new_tenant_seed_write_does_not_leave_a_broken_tenant() {
    let state = State::new();
    assert!(state.run(&["init"]).status.success());
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(state.0.join("data/state.sqlite")),
    )
    .await
    .unwrap();
    sqlx::query("CREATE TRIGGER fail_seed BEFORE INSERT ON secrets BEGIN SELECT RAISE(ABORT,'synthetic seed failure'); END")
        .execute(&pool).await.unwrap();
    assert!(!state.run(&["new-tenant"]).status.success());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tenants")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    sqlx::query("DROP TRIGGER fail_seed")
        .execute(&pool)
        .await
        .unwrap();
    assert!(state.run(&["new-tenant"]).status.success());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tenants")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
    pool.close().await;
}

#[tokio::test]
async fn backup_record_inspection_outputs_only_the_verified_client_id_without_network() {
    use bridge_core::{
        device, identity,
        model::{AccountIdentity, EndpointKind},
        vault::{SecretScope, Vault},
    };
    let state = State::new();
    let initialized = state.run(&["init"]);
    assert!(initialized.status.success());
    let stdout = String::from_utf8(initialized.stdout).unwrap();
    let tenant: Uuid = stdout
        .split("Tenant：")
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let store = std::sync::Arc::new(
        bridge_core::storage::SqliteStore::open(
            sqlx::sqlite::SqliteConnectOptions::new().filename(state.0.join("data/state.sqlite")),
        )
        .await
        .unwrap(),
    );
    let vault = Vault::new(
        store.clone(),
        bridge_server::key_file::load(&state.0.join("keys/master.key")).unwrap(),
    );
    let account = AccountIdentity {
        workspace_id: "synthetic-workspace".into(),
        user_id: "synthetic-user".into(),
    };
    store.create_account(tenant, &account).await.unwrap();
    let seed = vault.tenant_seed(tenant).await.unwrap();
    let endpoint = identity::endpoint(
        &seed,
        tenant,
        account.clone(),
        "backup-out",
        EndpointKind::Out,
    )
    .unwrap();
    store.create_endpoint(&endpoint).await.unwrap();
    let key = identity::signing_key(&seed, tenant, &account, "backup-out").unwrap();
    let registration = device::Registration {
        client_id: "original-client".into(),
        account_user_id: "synthetic-user".into(),
        key_id: device::identity(&key).key_id,
    };
    vault
        .save_out_registration(tenant, endpoint.device_id, &registration)
        .await
        .unwrap();
    drop(vault);
    std::sync::Arc::try_unwrap(store)
        .ok()
        .unwrap()
        .close()
        .await;
    let run = |id: Uuid| {
        Command::new(env!("CARGO_BIN_EXE_bridge-admin"))
            .arg(state.0.join("data/state.sqlite"))
            .arg(state.0.join("keys/master.key"))
            .args([
                "show-out-record",
                &id.to_string(),
                &endpoint.device_id.to_string(),
            ])
            // Proves this offline path does not even construct the upstream adapter.
            .env("BRIDGE_UPSTREAM_PROXY", "invalid proxy URL")
            .output()
            .unwrap()
    };
    let result = run(tenant);
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "原注册 client ID：original-client\n"
    );
    assert!(!run(Uuid::new_v4()).status.success());
    let store = std::sync::Arc::new(
        bridge_core::storage::SqliteStore::open(
            sqlx::sqlite::SqliteConnectOptions::new().filename(state.0.join("data/state.sqlite")),
        )
        .await
        .unwrap(),
    );
    let vault = Vault::new(
        store.clone(),
        bridge_server::key_file::load(&state.0.join("keys/master.key")).unwrap(),
    );
    let wrong = device::Registration {
        key_id: "foreign-key".into(),
        ..registration
    };
    vault
        .write(
            tenant,
            &SecretScope::OutRegistration(endpoint.device_id),
            &serde_json::to_vec(&wrong).unwrap(),
        )
        .await
        .unwrap();
    drop(vault);
    std::sync::Arc::try_unwrap(store)
        .ok()
        .unwrap()
        .close()
        .await;
    assert!(!run(tenant).status.success());
}

#[test]
fn admin_bootstraps_and_persists_pkce_without_contacting_upstream() {
    let state = State::new();
    let initialized = state.run(&["init"]);
    assert!(initialized.status.success());
    let stdout = String::from_utf8(initialized.stdout).unwrap();
    let tenant = stdout.split("Tenant：").nth(1).unwrap().trim();
    Uuid::parse_str(tenant).unwrap();
    assert!(!state.run(&["init"]).status.success());
    let login = state.run(&["login", tenant]);
    assert!(login.status.success());
    let stdout = String::from_utf8(login.stdout).unwrap();
    assert!(stdout.contains("https://auth.openai.com/oauth/authorize?"));
    assert!(stdout.contains("code_challenge_method=S256"));
    assert!(!stdout.contains("code_verifier="));
    assert!(
        !state
            .run(&["login", &Uuid::new_v4().to_string()])
            .status
            .success()
    );
    std::fs::remove_file(state.0.join("keys/master.key")).unwrap();
    assert!(!state.run(&["login", tenant]).status.success());
    assert!(!state.0.join("keys/master.key").exists());
}

#[tokio::test]
async fn operator_can_add_an_isolated_tenant_without_replacing_existing_identity() {
    let state = State::new();
    let initialized = state.run(&["init"]);
    assert!(initialized.status.success());
    let stdout = String::from_utf8(initialized.stdout).unwrap();
    let original: Uuid = stdout
        .split("Tenant：")
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let created = state.run(&["new-tenant"]);
    assert!(created.status.success());
    let stdout = String::from_utf8(created.stdout).unwrap();
    let second: Uuid = stdout
        .split("Tenant：")
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(original, second);
    let store = std::sync::Arc::new(
        bridge_core::storage::SqliteStore::open(
            sqlx::sqlite::SqliteConnectOptions::new().filename(state.0.join("data/state.sqlite")),
        )
        .await
        .unwrap(),
    );
    let vault = bridge_core::vault::Vault::new(
        store.clone(),
        bridge_server::key_file::load(&state.0.join("keys/master.key")).unwrap(),
    );
    let one = vault
        .read(original, &bridge_core::vault::SecretScope::TenantSeed)
        .await
        .unwrap()
        .unwrap();
    let two = vault
        .read(second, &bridge_core::vault::SecretScope::TenantSeed)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(&*one, &*two);
    assert_eq!(store.local_tenants().await.unwrap().len(), 2);
    drop(vault);
    std::sync::Arc::try_unwrap(store)
        .ok()
        .unwrap()
        .close()
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_account_does_not_report_fresh_cached_credentials_valid_without_upstream() {
    use bridge_core::{
        model::AccountIdentity,
        vault::{SecretScope, Vault},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let state = State::new();
    let initialized = state.run(&["init"]);
    assert!(initialized.status.success());
    let stdout = String::from_utf8(initialized.stdout).unwrap();
    let tenant: Uuid = stdout
        .split("Tenant：")
        .nth(1)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let store = std::sync::Arc::new(
        bridge_core::storage::SqliteStore::open(
            sqlx::sqlite::SqliteConnectOptions::new().filename(state.0.join("data/state.sqlite")),
        )
        .await
        .unwrap(),
    );
    let vault = Vault::new(
        store.clone(),
        bridge_server::key_file::load(&state.0.join("keys/master.key")).unwrap(),
    );
    let account = AccountIdentity {
        workspace_id: "synthetic-workspace".into(),
        user_id: "synthetic-user".into(),
    };
    store.create_account(tenant, &account).await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    vault.write(tenant, &SecretScope::Account(account.clone()), &serde_json::to_vec(&serde_json::json!({
        "account":account,"access":"synthetic-cached-access","refresh":"synthetic-refresh","global_user":null,"auth_user":null,"expires_at":now+3600,
    })).unwrap()).await.unwrap();
    drop(vault);
    std::sync::Arc::try_unwrap(store)
        .ok()
        .unwrap()
        .close()
        .await;
    // Fake local proxy rejects CONNECT; this test cannot reach a real upstream account.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let served = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = served.clone();
    let server = tokio::spawn(async move {
        if let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await
        {
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
            }
            assert!(request.starts_with(b"CONNECT chatgpt.com:443 HTTP/1.1\r\n"));
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
            stream
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        }
    });
    let output = Command::new(env!("CARGO_BIN_EXE_bridge-admin"))
        .arg(state.0.join("data/state.sqlite"))
        .arg(state.0.join("keys/master.key"))
        .args([
            "check-account",
            &tenant.to_string(),
            &account.workspace_id,
            &account.user_id,
        ])
        .env("BRIDGE_UPSTREAM_PROXY", proxy)
        .output()
        .unwrap();
    server.await.unwrap();
    assert!(
        served.load(std::sync::atomic::Ordering::SeqCst),
        "fresh cache must still be checked against upstream"
    );
    assert!(!output.status.success());
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("账号授权有效")
    );
}
