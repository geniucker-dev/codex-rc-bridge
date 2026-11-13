use super::*;
use crate::{
    device::Registration,
    identity,
    upstream::HostRegistration,
    vault::{EncryptionKey, SecretScope, Vault},
};
use std::{path::PathBuf, sync::Arc};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("bridge-storage-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn options(&self) -> SqliteConnectOptions {
        SqliteConnectOptions::new()
            .filename(self.0.join("state.sqlite"))
            .create_if_missing(true)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn disk_policy_overrides_unsafe_options_and_survives_connection_replacement() {
    let temp = Temp::new();
    let store = SqliteStore::open(
        temp.options()
            .synchronous(SqliteSynchronous::Off)
            .foreign_keys(false),
    )
    .await
    .unwrap();
    for _ in 0..2 {
        let mut connection = store.pool.acquire().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
                .fetch_one(&mut *connection)
                .await
                .unwrap(),
            "delete"
        );
        for (pragma, expected) in [
            ("PRAGMA synchronous", 3),
            ("PRAGMA foreign_keys", 1),
            ("PRAGMA fullfsync", 1),
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(pragma)
                    .fetch_one(&mut *connection)
                    .await
                    .unwrap(),
                expected
            );
        }
        connection.close().await.unwrap();
    }
    store.close().await;
}

#[tokio::test]
async fn stopped_database_backup_restores_identity_binding_cleanup_and_retired_streams() {
    let source = Temp::new();
    let backup = Temp::new();
    let store = Arc::new(SqliteStore::open(source.options()).await.unwrap());
    let tenant = Uuid::new_v4();
    let foreign = Uuid::new_v4();
    for id in [tenant, foreign] {
        store.create_tenant(id).await.unwrap();
    }
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
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
    let pending =
        identity::endpoint(&seed, tenant, account.clone(), "pending", EndpointKind::Out).unwrap();
    for endpoint in [&input, &output, &pending] {
        store.create_endpoint(endpoint).await.unwrap();
    }
    let key = identity::signing_key(&seed, tenant, &account, "desk").unwrap();
    let registration = Registration {
        client_id: "synthetic-client".into(),
        account_user_id: "synthetic-user".into(),
        key_id: "synthetic-key".into(),
    };
    vault
        .save_out_registration(tenant, output.device_id, &registration)
        .await
        .unwrap();
    vault
        .save_host_registration(
            tenant,
            input.device_id,
            &HostRegistration {
                server_id: "synthetic-server".into(),
                environment_id: "synthetic-in".into(),
            },
        )
        .await
        .unwrap();
    store
        .set_target(
            tenant,
            output.device_id,
            &crate::model::Target {
                environment_id: "synthetic-b".into(),
                app_server_version: "test/1".into(),
            },
        )
        .await
        .unwrap();
    let binding = Binding {
        tenant_id: tenant,
        in_id: input.device_id,
        out_id: output.device_id,
    };
    store.bind(&binding).await.unwrap();
    store
        .set_chain_enabled(tenant, input.device_id, true)
        .await
        .unwrap();
    store
        .begin_enrollment(tenant, pending.device_id)
        .await
        .unwrap();
    vault
        .save_out_enrollment(tenant, pending.device_id, &registration)
        .await
        .unwrap();
    store.begin_delete(&pending).await.unwrap();
    assert!(
        store
            .claim_stream(tenant, input.device_id, "synthetic-stream-hash")
            .await
            .unwrap()
    );
    drop(vault);
    Arc::try_unwrap(store).ok().unwrap().close().await;
    // Offline backup: no live connection, WAL or hot journal is discarded.
    std::fs::copy(source.0.join("state.sqlite"), backup.0.join("state.sqlite")).unwrap();
    drop(source);
    let restored = Arc::new(SqliteStore::open(backup.options()).await.unwrap());
    let wrong = Vault::new(
        restored.clone(),
        EncryptionKey::from_bytes(&[20; 32]).unwrap(),
    );
    assert!(wrong.read(tenant, &SecretScope::TenantSeed).await.is_err());
    let vault = Vault::new(
        restored.clone(),
        EncryptionKey::from_bytes(&[19; 32]).unwrap(),
    );
    let seed = vault.tenant_seed(tenant).await.unwrap();
    assert_eq!(
        identity::endpoint(&seed, tenant, account.clone(), "desk", EndpointKind::Out).unwrap(),
        output
    );
    assert_eq!(
        identity::signing_key(&seed, tenant, &account, "desk")
            .unwrap()
            .public_key(),
        key.public_key()
    );
    assert_eq!(
        restored
            .binding(tenant, input.device_id)
            .await
            .unwrap()
            .unwrap()
            .out_id,
        output.device_id
    );
    assert!(
        restored
            .chain_enabled(tenant, input.device_id)
            .await
            .unwrap()
    );
    assert_eq!(
        restored
            .target(tenant, output.device_id)
            .await
            .unwrap()
            .unwrap()
            .environment_id,
        "synthetic-b"
    );
    assert!(
        restored
            .endpoint_deleting(tenant, pending.device_id)
            .await
            .unwrap()
    );
    assert!(
        vault
            .read(tenant, &SecretScope::OutEnrollment(pending.device_id))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        restored
            .begin_enrollment(tenant, pending.device_id)
            .await
            .is_err()
    );
    assert!(restored.create_endpoint(&pending).await.is_err());
    assert!(
        !restored
            .claim_stream(tenant, input.device_id, "synthetic-stream-hash")
            .await
            .unwrap()
    );
    assert!(restored.bind(&binding).await.is_err());
    assert!(
        restored
            .binding(foreign, input.device_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        vault
            .read(foreign, &SecretScope::OutEnrollment(pending.device_id))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
            .fetch_one(&restored.pool)
            .await
            .unwrap(),
        "ok"
    );
    drop((wrong, vault));
    Arc::try_unwrap(restored).ok().unwrap().close().await;
}

// A separately spawned test process is killed with an open, spilled transaction.
#[test]
#[ignore = "helper invoked by killed_writer_rolls_back_only_uncommitted_changes"]
fn crash_writer_process() {
    let root = PathBuf::from(std::env::var_os("BRIDGE_STORAGE_TEST_ROOT").unwrap());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let store =
            SqliteStore::open(SqliteConnectOptions::new().filename(root.join("state.sqlite")))
                .await
                .unwrap();
        store.create_tenant(Uuid::from_u128(42)).await.unwrap();
        assert!(
            store
                .claim_stream(Uuid::from_u128(42), Uuid::from_u128(43), "committed-stream")
                .await
                .unwrap()
        );
        sqlx::query("CREATE TABLE crash_probe(id INTEGER PRIMARY KEY, body BLOB)")
            .execute(&store.pool)
            .await
            .unwrap();
        sqlx::query("PRAGMA cache_size=5")
            .execute(&store.pool)
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        sqlx::query("DELETE FROM used_streams")
            .execute(&mut *tx)
            .await
            .unwrap();
        for id in 0..64 {
            sqlx::query("INSERT INTO crash_probe VALUES(?,zeroblob(4096))")
                .bind(id)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        std::fs::write(root.join("ready"), b"ready").unwrap();
        std::future::pending::<()>().await;
    });
}

#[tokio::test]
async fn killed_writer_rolls_back_only_uncommitted_changes() {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = Temp::new();
    SqliteStore::open(temp.options())
        .await
        .unwrap()
        .close()
        .await;
    let mut child = Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "storage::tests::crash_writer_process",
                "--ignored",
            ])
            .env("BRIDGE_STORAGE_TEST_ROOT", &temp.0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while !temp.0.join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "writer exited before ready"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        std::fs::metadata(temp.0.join("state.sqlite-journal"))
            .unwrap()
            .len()
            > 512
    );
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let store = SqliteStore::open(temp.options()).await.unwrap();
    assert_eq!(
        store.local_tenants().await.unwrap(),
        vec![Uuid::from_u128(42)]
    );
    assert!(
        !store
            .claim_stream(Uuid::from_u128(42), Uuid::from_u128(43), "committed-stream")
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crash_probe")
            .fetch_one(&store.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
            .fetch_one(&store.pool)
            .await
            .unwrap(),
        "ok"
    );
    store.close().await;
}

#[tokio::test]
async fn target_record_follows_only_the_selected_environment() {
    let dir = Temp::new();
    let store = Arc::new(SqliteStore::open(dir.options()).await.unwrap());
    let tenant = Uuid::new_v4();
    store.create_tenant(tenant).await.unwrap();
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[23; 32]).unwrap());
    let seed = vault.tenant_seed(tenant).await.unwrap();
    let account = AccountIdentity {
        workspace_id: "synthetic-workspace".into(),
        user_id: "synthetic-user".into(),
    };
    store.create_account(tenant, &account).await.unwrap();
    let output = identity::endpoint(&seed, tenant, account, "desk", EndpointKind::Out).unwrap();
    store.create_endpoint(&output).await.unwrap();
    let key = identity::signing_key(&seed, tenant, &output.account, "desk").unwrap();
    vault
        .save_out_registration(
            tenant,
            output.device_id,
            &Registration {
                client_id: "synthetic-client".into(),
                account_user_id: "synthetic-user".into(),
                key_id: crate::device::identity(&key).key_id,
            },
        )
        .await
        .unwrap();
    let select = |environment: &'static str| crate::model::Target {
        environment_id: environment.into(),
        app_server_version: "test/1".into(),
    };
    store
        .set_target(tenant, output.device_id, &select("b"))
        .await
        .unwrap();
    assert_eq!(
        store
            .target_record(tenant, output.device_id)
            .await
            .unwrap()
            .name,
        None
    );
    store
        .observe_target(
            tenant,
            output.device_id,
            "b",
            &crate::output_sync::TargetObservation {
                record: Some(crate::output_sync::TargetRecord {
                    name: Some("MacBook Pro".into()),
                    online: Some(true),
                    last_seen_at: Some(123),
                    error: None,
                }),
                error: None,
            },
        )
        .await
        .unwrap();
    // A late label for another environment never renames the current target.
    store
        .observe_target(
            tenant,
            output.device_id,
            "c",
            &crate::output_sync::TargetObservation {
                record: Some(crate::output_sync::TargetRecord {
                    name: Some("Stale".into()),
                    online: Some(false),
                    last_seen_at: Some(0),
                    error: None,
                }),
                error: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .target_record(tenant, output.device_id)
            .await
            .unwrap()
            .name
            .as_deref(),
        Some("MacBook Pro")
    );
    let record = store.target_record(tenant, output.device_id).await.unwrap();
    assert_eq!(record.online, Some(true));
    assert_eq!(record.last_seen_at, Some(123));
    // Reselecting the same device keeps its name; choosing another hides it.
    store
        .set_target(tenant, output.device_id, &select("b"))
        .await
        .unwrap();
    assert!(
        store
            .target_record(tenant, output.device_id)
            .await
            .unwrap()
            .name
            .is_some()
    );
    store
        .set_target(tenant, output.device_id, &select("c"))
        .await
        .unwrap();
    assert_eq!(
        store
            .target_record(tenant, output.device_id)
            .await
            .unwrap()
            .name,
        None
    );
}
