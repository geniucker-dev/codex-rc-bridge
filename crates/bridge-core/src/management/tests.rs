use super::*;
use sqlx::sqlite::SqliteConnectOptions;

async fn fixture() -> (Arc<SqliteStore>, ManagementAuth, Uuid) {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let tenant = Uuid::new_v4();
    store.create_tenant(tenant).await.unwrap();
    let auth = ManagementAuth::new(store.clone());
    assert!(!auth.has_users().await.unwrap());
    auth.create_user(tenant, "alice", secret("synthetic-password-one"))
        .await
        .unwrap();
    assert!(auth.has_users().await.unwrap());
    (store, auth, tenant)
}
fn secret(value: &str) -> SecretString {
    SecretString::new(value.into())
}

#[tokio::test]
async fn sessions_are_hashed_expire_and_reset_only_affects_the_owned_user() {
    let (store, auth, tenant) = fixture().await;
    let second = Uuid::new_v4();
    store.create_tenant(second).await.unwrap();
    auth.create_user(second, "bob", secret("synthetic-password-two"))
        .await
        .unwrap();
    let alice = auth
        .login("alice", secret("synthetic-password-one"), 100)
        .await
        .unwrap();
    let bob = auth
        .login("bob", secret("synthetic-password-two"), 100)
        .await
        .unwrap();
    assert_eq!(
        auth.session(alice.token.expose(), 101)
            .await
            .unwrap()
            .tenant_id,
        tenant
    );
    assert_eq!(
        auth.session(bob.token.expose(), 101)
            .await
            .unwrap()
            .tenant_id,
        second
    );
    let hash: String =
        sqlx::query_scalar("SELECT password_hash FROM management_users WHERE username='alice'")
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert!(hash.starts_with("$argon2id$"));
    assert!(!hash.contains("synthetic-password"));
    let stored: Vec<String> = sqlx::query_scalar("SELECT token_hash FROM management_sessions")
        .fetch_all(&store.pool)
        .await
        .unwrap();
    assert!(
        !stored
            .iter()
            .any(|h| h == alice.token.expose() || h == bob.token.expose())
    );
    let csrf = csrf_token(alice.token.expose()).unwrap();
    assert!(!stored.contains(&csrf));
    assert!(auth.session(&csrf, 101).await.is_err());
    assert!(
        auth.session(alice.token.expose(), 100 + SESSION_SECONDS)
            .await
            .is_err()
    );
    assert!(
        auth.reset_password(second, "alice", secret("replacement-password"))
            .await
            .is_err()
    );
    auth.reset_password(tenant, "alice", secret("replacement-password"))
        .await
        .unwrap();
    assert!(auth.session(alice.token.expose(), 101).await.is_err());
    assert!(auth.session(bob.token.expose(), 101).await.is_ok());
    assert!(
        auth.login("alice", secret("synthetic-password-one"), 102)
            .await
            .is_err()
    );
    let new = auth
        .login("alice", secret("replacement-password"), 102)
        .await
        .unwrap();
    auth.logout(new.token.expose()).await.unwrap();
    assert!(auth.session(new.token.expose(), 103).await.is_err());
    assert!(auth.session("not-a-token", 103).await.is_err());
}

#[tokio::test]
async fn lockout_session_cap_and_password_validation_are_enforced() {
    let (_, auth, tenant) = fixture().await;
    assert!(
        auth.create_user(tenant, "bad", secret("四个字符"))
            .await
            .is_err()
    );
    assert!(matches!(
        auth.login("unknown", secret("synthetic-password-one"), 100)
            .await,
        Err(ManagementError::Unauthorized)
    ));
    for _ in 0..5 {
        assert!(
            auth.login("alice", secret("wrong-password-0000"), 100)
                .await
                .is_err()
        );
    }
    assert!(
        auth.login("alice", secret("synthetic-password-one"), 159)
            .await
            .is_err()
    );
    let first = auth
        .login("alice", secret("synthetic-password-one"), 160)
        .await
        .unwrap();
    for now in 161..171 {
        auth.login("alice", secret("synthetic-password-one"), now)
            .await
            .unwrap();
    }
    assert!(auth.session(first.token.expose(), 171).await.is_err());
    // Password verification is bounded even for nonexistent users, separately from per-user lockout.
    for _ in 0..12 {
        let _ = auth
            .login("unknown", secret("wrong-password-0000"), 172)
            .await;
    }
    assert!(matches!(
        auth.login("alice", secret("synthetic-password-one"), 173)
            .await,
        Err(ManagementError::Limited)
    ));
}

#[tokio::test]
async fn web_password_changes_retain_only_current_session_and_count_lockout() {
    let (store, auth, tenant) = fixture().await;
    let a = auth
        .login("alice", secret("synthetic-password-one"), 100)
        .await
        .unwrap();
    let b = auth
        .login("alice", secret("synthetic-password-one"), 100)
        .await
        .unwrap();
    let old: String =
        sqlx::query_scalar("SELECT password_hash FROM management_users WHERE tenant_id=?")
            .bind(tenant.to_string())
            .fetch_one(&store.pool)
            .await
            .unwrap();
    auth.change_password(
        a.token.expose(),
        secret("synthetic-password-one"),
        secret("replacement-password"),
        101,
    )
    .await
    .unwrap();
    assert!(auth.session(a.token.expose(), 102).await.is_ok());
    assert!(auth.session(b.token.expose(), 102).await.is_err());
    // A verifier that finished against the old hash cannot overwrite the newer password.
    assert!(matches!(
        auth.commit_password_change(a.token.expose(), a.identity.user_id, &old, &old, 102)
            .await,
        Err(ManagementError::CurrentPasswordIncorrect)
    ));
    for _ in 0..5 {
        assert!(matches!(
            auth.change_password(
                a.token.expose(),
                secret("incorrect"),
                secret("another-password"),
                103
            )
            .await,
            Err(ManagementError::CurrentPasswordIncorrect)
        ));
    }
    assert!(matches!(
        auth.change_password(
            a.token.expose(),
            secret("replacement-password"),
            secret("another-password"),
            104
        )
        .await,
        Err(ManagementError::Limited)
    ));
    // Administrative reset revokes even a previously verified current session.
    let hash: String =
        sqlx::query_scalar("SELECT password_hash FROM management_users WHERE tenant_id=?")
            .bind(tenant.to_string())
            .fetch_one(&store.pool)
            .await
            .unwrap();
    auth.reset_password(tenant, "alice", secret("admin-replacement"))
        .await
        .unwrap();
    assert!(matches!(
        auth.commit_password_change(a.token.expose(), a.identity.user_id, &hash, &old, 105)
            .await,
        Err(ManagementError::Unauthorized)
    ));
    assert!(
        auth.login("alice", secret("admin-replacement"), 106)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn deleting_populated_space_respects_switch_triggers_and_leaves_other_space_intact() {
    use crate::vault::{EncryptionKey, Vault};
    let (store, auth, _) = fixture().await;
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[7; 32]).unwrap());
    let (id, tenant) = auth
        .add_web_user(&vault, "bob", secret("synthetic-password-two"))
        .await
        .unwrap();
    let t = tenant.to_string();
    let statements = [
        "INSERT INTO accounts(tenant_id,workspace_id,user_id) VALUES(?,'w','u')",
        "INSERT INTO endpoints(tenant_id,device_id,workspace_id,user_id,kind,identity_name,display_name) VALUES(?,'i','w','u','in','i','i'),(?,'o','w','u','out','o','o'),(?,'n','w','u','out','n','n')",
        "INSERT INTO bindings(tenant_id,in_id,out_id) VALUES(?,'i','o')",
        "INSERT INTO chain_settings VALUES(?,'i',1)",
        "INSERT INTO targets VALUES(?,'n','e','v')",
        "INSERT INTO chain_switches(tenant_id,in_id,old_out_id,new_out_id) VALUES(?,'i','o','n')",
        "INSERT INTO host_environments VALUES(?,'i','host')",
        "INSERT INTO oauth_transactions VALUES(?,'login',999999,x'01')",
        "INSERT INTO used_streams VALUES(?,'i','hash')",
        "INSERT INTO enrollment_attempts VALUES(?,'o')",
        "INSERT INTO target_records(tenant_id,out_id,environment_id) VALUES(?,'o','e')",
        "INSERT INTO output_pairings VALUES(?,'o')",
        "INSERT INTO input_devices VALUES(?,'i','{}')",
        "INSERT INTO input_pairings(tenant_id,in_id,environment_id) VALUES(?,'i','host')",
    ];
    for statement in statements {
        let mut q = sqlx::query(statement);
        for _ in 0..statement.matches('?').count() {
            q = q.bind(&t);
        }
        q.execute(&store.pool).await.unwrap();
    }
    let session = auth
        .login("bob", secret("synthetic-password-two"), 100)
        .await
        .unwrap();
    // A late failure must restore switch reservations, bindings and credentials together.
    sqlx::query("CREATE TRIGGER reject_tenant_delete BEFORE DELETE ON accounts BEGIN SELECT RAISE(ABORT,'synthetic deletion failure'); END").execute(&store.pool).await.unwrap();
    assert!(matches!(
        auth.delete_web_user(id).await,
        Err(ManagementError::Storage)
    ));
    assert!(auth.session(session.token.expose(), 101).await.is_ok());
    for table in ["chain_switches", "bindings", "targets", "secrets"] {
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE tenant_id=?"))
                .bind(&t)
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "failed deletion must roll back {table}");
    }
    sqlx::query("DROP TRIGGER reject_tenant_delete")
        .execute(&store.pool)
        .await
        .unwrap();
    auth.delete_web_user(id).await.unwrap();
    assert!(auth.session(session.token.expose(), 101).await.is_err());
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND sql LIKE '%tenant_id%'",
    )
    .fetch_all(&store.pool)
    .await
    .unwrap();
    for table in tables {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE tenant_id=?"))
            .bind(&t)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table} leaked deleted space data");
    }
    assert!(
        auth.login("alice", secret("synthetic-password-one"), 101)
            .await
            .is_ok()
    );
    assert_eq!(store.local_tenants().await.unwrap().len(), 1);
    let admin = auth.users(Uuid::nil(), 100).await.unwrap().remove(0).id;
    assert!(matches!(
        auth.delete_web_user(admin).await,
        Err(ManagementError::TargetIsAdmin)
    ));
}

#[tokio::test]
async fn reset_wins_over_inflight_old_login_and_self_change() {
    for own_change in [false, true] {
        let (store, auth, _) = fixture().await;
        let vault = crate::vault::Vault::new(
            store,
            crate::vault::EncryptionKey::from_bytes(&[31; 32]).unwrap(),
        );
        let (_, _) = auth
            .add_web_user(&vault, "bob", secret("synthetic-password-two"))
            .await
            .unwrap();
        let bob = auth
            .login("bob", secret("synthetic-password-two"), 100)
            .await
            .unwrap();
        let id = bob.identity.user_id;
        let auth = Arc::new(auth);
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, resume) = tokio::sync::oneshot::channel();
        *auth.verification_gate.lock().await = Some(VerificationGate {
            entered,
            release: resume,
        });
        let other = auth.clone();
        let token = bob.token.expose().to_owned();
        let request = tokio::spawn(async move {
            if own_change {
                other
                    .change_password(
                        &token,
                        secret("synthetic-password-two"),
                        secret("own-new-password"),
                        101,
                    )
                    .await
            } else {
                other
                    .login("bob", secret("synthetic-password-two"), 101)
                    .await
                    .map(|_| ())
            }
        });
        ready.await.unwrap();
        auth.reset_web_password(id, secret("admin-new-password"))
            .await
            .unwrap();
        release.send(()).unwrap();
        assert!(matches!(
            request.await.unwrap(),
            Err(ManagementError::Unauthorized)
        ));
        assert!(auth.session(bob.token.expose(), 102).await.is_err());
        assert!(
            auth.login("bob", secret("admin-new-password"), 102)
                .await
                .is_ok()
        );
        assert!(
            auth.login("bob", secret("own-new-password"), 102)
                .await
                .is_err()
        );
        assert!(
            auth.login("bob", secret("synthetic-password-two"), 102)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn admin_hashing_queues_force_reset_overwrites_and_failed_creation_rolls_back() {
    let (store, auth, _) = fixture().await;
    let vault = crate::vault::Vault::new(
        store.clone(),
        crate::vault::EncryptionKey::from_bytes(&[33; 32]).unwrap(),
    );
    let permits = auth.workers.clone().acquire_many_owned(2).await.unwrap();
    let add = auth.add_web_user(&vault, "bob", secret("synthetic-password-two"));
    tokio::pin!(add);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut add)
            .await
            .is_err(),
        "admin creation must wait for hashing capacity"
    );
    drop(permits);
    let (id, _) = add.await.unwrap();
    let bob = auth
        .login("bob", secret("synthetic-password-two"), 100)
        .await
        .unwrap();
    let own_hash = hash_password(secret("own-new-password")).await.unwrap();
    let permits = auth.workers.clone().acquire_many_owned(2).await.unwrap();
    assert!(
        matches!(
            auth.change_password(
                bob.token.expose(),
                secret("synthetic-password-two"),
                secret("own-new-password"),
                101
            )
            .await,
            Err(ManagementError::Limited)
        ),
        "self-change must still fail immediately"
    );
    let reset = auth.reset_web_password(id, secret("admin-new-password"));
    tokio::pin!(reset);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut reset)
            .await
            .is_err(),
        "admin reset must wait for hashing capacity"
    );
    // Model a self-change committed while reset was queued; reset must overwrite it.
    sqlx::query("UPDATE management_users SET password_hash=? WHERE id=?")
        .bind(own_hash)
        .bind(id.to_string())
        .execute(&store.pool)
        .await
        .unwrap();
    drop(permits);
    reset.await.unwrap();
    assert!(auth.session(bob.token.expose(), 102).await.is_err());
    assert!(
        auth.login("bob", secret("admin-new-password"), 102)
            .await
            .is_ok()
    );
    assert!(
        auth.login("bob", secret("own-new-password"), 102)
            .await
            .is_err()
    );
    auth.delete_web_user(id).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_web_user BEFORE INSERT ON management_users WHEN NEW.username='bob' BEGIN SELECT RAISE(ABORT,'synthetic write failure'); END").execute(&store.pool).await.unwrap();
    assert!(matches!(
        auth.add_web_user(&vault, "bob", secret("synthetic-password-two"))
            .await,
        Err(ManagementError::Storage)
    ));
    assert_eq!(store.local_tenants().await.unwrap().len(), 1);
    let seeds: i64 = sqlx::query_scalar("SELECT count(*) FROM secrets")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(seeds, 0);
}
