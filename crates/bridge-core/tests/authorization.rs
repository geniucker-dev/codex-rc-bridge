use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bridge_core::{
    auth::{
        AccountTokens, AuthError, AuthService, LoginPurpose, LoginResult, SecretString,
        TokenEndpoint, TokenResponse, validate_step_up,
    },
    identity::{self, TenantSeed},
    model::{AccountIdentity, EndpointKind},
    storage::SqliteStore,
    vault::{EncryptionKey, SecretScope, Vault},
};
use serde_json::{Value, json};
use sqlx::sqlite::SqliteConnectOptions;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Mutex;
use uuid::Uuid;

const NOW: i64 = 1800000000;
fn account() -> AccountIdentity {
    AccountIdentity {
        workspace_id: "workspace-example".into(),
        user_id: "member-example".into(),
    }
}
fn token_claims(exp: i64) -> Value {
    json!({"exp":exp,"sub":"global-example","https://api.openai.com/auth":{
    "chatgpt_account_id":"workspace-example","chatgpt_account_user_id":"member-example","user_id":"global-example"}})
}
fn token(claims: Value) -> SecretString {
    SecretString::new(format!(
        "e30.{}.synthetic",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    ))
}
fn response(claims: Value) -> TokenResponse {
    TokenResponse {
        access_token: token(claims),
        refresh_token: Some(SecretString::new("synthetic-refresh".into())),
    }
}

type CapturedForms = Arc<Mutex<Vec<Vec<(String, String)>>>>;
#[derive(Clone)]
struct FakeToken {
    calls: Arc<AtomicUsize>,
    failure: Arc<Mutex<Option<AuthError>>>,
    next: Arc<Mutex<Value>>,
    forms: CapturedForms,
}
impl FakeToken {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            failure: Arc::new(Mutex::new(None)),
            next: Arc::new(Mutex::new(token_claims(NOW + 3600))),
            forms: Arc::new(Mutex::new(Vec::new())),
        }
    }
}
impl TokenEndpoint for FakeToken {
    async fn token(&self, form: &[(&str, &str)]) -> Result<TokenResponse, AuthError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.forms.lock().await.push(
            form.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        if let Some(error) = self.failure.lock().await.take() {
            return Err(error);
        }
        tokio::task::yield_now().await;
        Ok(response(self.next.lock().await.clone()))
    }
}
async fn setup() -> (
    Arc<SqliteStore>,
    Vault,
    Uuid,
    FakeToken,
    AuthService<FakeToken>,
) {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let tenant = Uuid::from_u128(1);
    store.create_tenant(tenant).await.unwrap();
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[11; 32]).unwrap());
    let fake = FakeToken::new();
    let auth = AuthService::new(vault.clone(), fake.clone());
    (store, vault, tenant, fake, auth)
}
fn callback(start: &bridge_core::auth::LoginStart) -> String {
    let authorize = url::Url::parse(&start.url).unwrap();
    let state = authorize
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1;
    format!("http://localhost:1455/auth/callback?code=synthetic-code&state={state}")
}

#[tokio::test]
async fn account_and_step_up_authorization_identify_the_bridge() {
    let (store, vault, tenant, _, auth) = setup().await;
    store.create_account(tenant, &account()).await.unwrap();
    let seed = vault.tenant_seed(tenant).await.unwrap();
    let endpoint = identity::endpoint(&seed, tenant, account(), "desk", EndpointKind::Out).unwrap();
    store.create_endpoint(&endpoint).await.unwrap();
    for purpose in [
        LoginPurpose::Account { expected: None },
        LoginPurpose::OutStepUp {
            device_id: endpoint.device_id,
            account: account(),
        },
    ] {
        let step_up = matches!(purpose, LoginPurpose::OutStepUp { .. });
        for port in [1455, 1457] {
            let start = auth
                .begin(tenant, purpose.clone(), port, NOW)
                .await
                .unwrap();
            let url = url::Url::parse(&start.url).unwrap();
            let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
            assert_eq!(query.get("originator").unwrap(), "codex_rc_bridge");
            assert_eq!(
                query.get("client_id").unwrap(),
                bridge_core::auth::OAUTH_CLIENT_ID
            );
            assert_eq!(
                query.get("redirect_uri").unwrap(),
                &format!("http://localhost:{port}/auth/callback")
            );
            assert_eq!(
                query.get("reauth").map(|value| value.as_ref()),
                step_up.then_some("remote_control")
            );
        }
    }
}

async fn login(auth: &AuthService<FakeToken>, tenant: Uuid) -> AccountTokens {
    let start = auth
        .begin(
            tenant,
            LoginPurpose::Account {
                expected: Some(account()),
            },
            1455,
            NOW,
        )
        .await
        .unwrap();
    match auth
        .complete(tenant, start.id, &callback(&start), NOW)
        .await
        .unwrap()
    {
        LoginResult::Account(tokens) => tokens,
        _ => panic!("unexpected step-up"),
    }
}

#[test]
fn encrypted_records_bind_owner_tenant_purpose_and_reject_tampering() {
    let key = EncryptionKey::from_bytes(&[11; 32]).unwrap();
    let scope = SecretScope::Account(account());
    let tenant = Uuid::from_u128(1);
    let encrypted = key.seal(tenant, &scope, b"synthetic secret").unwrap();
    assert!(!encrypted.windows(16).any(|w| w == b"synthetic secret"));
    assert_ne!(
        encrypted,
        key.seal(tenant, &scope, b"synthetic secret").unwrap()
    );
    assert_eq!(
        key.open(tenant, &scope, &encrypted).unwrap().as_slice(),
        b"synthetic secret"
    );
    assert!(key.open(Uuid::from_u128(2), &scope, &encrypted).is_err());
    assert!(
        key.open(tenant, &SecretScope::TenantSeed, &encrypted)
            .is_err()
    );
    let other = SecretScope::Account(AccountIdentity {
        user_id: "another-member".into(),
        ..account()
    });
    assert!(key.open(tenant, &other, &encrypted).is_err());
    assert!(
        EncryptionKey::from_bytes(&[12; 32])
            .unwrap()
            .open(tenant, &scope, &encrypted)
            .is_err()
    );
    for index in [0, 1, 13, encrypted.len() - 1] {
        let mut tampered = encrypted.clone();
        tampered[index] ^= 1;
        assert!(key.open(tenant, &scope, &tampered).is_err());
    }
}

#[tokio::test]
async fn seed_creation_race_is_stable_and_cannot_be_overwritten() {
    let (_, vault, tenant, _, _) = setup().await;
    let (a, b) = tokio::join!(vault.tenant_seed(tenant), vault.tenant_seed(tenant));
    let id = |seed: &TenantSeed| {
        identity::endpoint(seed, tenant, account(), "desk", EndpointKind::Out)
            .unwrap()
            .device_id
    };
    assert_eq!(id(&a.unwrap()), id(&b.unwrap()));
    assert!(
        vault
            .write(tenant, &SecretScope::TenantSeed, &[0; 32])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn callback_validation_single_use_and_pkce_binding() {
    let (store, vault, tenant, fake, auth) = setup().await;
    let start = auth
        .begin(
            tenant,
            LoginPurpose::Account {
                expected: Some(account()),
            },
            1455,
            NOW,
        )
        .await
        .unwrap();
    let callback = callback(&start);
    let foreign = Uuid::from_u128(2);
    store.create_tenant(foreign).await.unwrap();
    assert!(matches!(
        auth.complete(foreign, start.id, &callback, NOW).await,
        Err(AuthError::Transaction)
    ));
    for bad in [
        callback.replace("localhost", "attacker.example"),
        callback.replace("http:", "https:"),
        callback.replace("1455", "1457"),
        callback.replace("/auth/callback", "/other"),
        format!("{callback}#fragment"),
        format!("{callback}&code=duplicate"),
        format!("{callback}&state=duplicate"),
        callback.replace("state=", "state=wrong"),
        callback.replace("localhost", "user@localhost"),
    ] {
        assert!(matches!(
            auth.complete(tenant, start.id, &bad, NOW).await,
            Err(AuthError::Callback)
        ));
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    let (a, b) = tokio::join!(
        auth.complete(tenant, start.id, &callback, NOW),
        auth.complete(tenant, start.id, &callback, NOW)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    let forms = fake.forms.lock().await;
    let verifier = forms[0]
        .iter()
        .find(|(k, _)| k == "code_verifier")
        .unwrap()
        .1
        .as_bytes();
    let challenge = bridge_core::device::digest_url(verifier);
    assert!(
        url::Url::parse(&start.url)
            .unwrap()
            .query_pairs()
            .any(|(k, v)| k == "code_challenge" && v == challenge)
    );
    assert!(
        vault
            .read(foreign, &SecretScope::Account(account()))
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        auth.complete(tenant, start.id, &callback, NOW).await,
        Err(AuthError::Transaction)
    ));
}

#[tokio::test]
async fn expired_declined_and_wrong_account_callbacks_do_not_save_tokens() {
    let (_, vault, tenant, fake, auth) = setup().await;
    let start = auth
        .begin(
            tenant,
            LoginPurpose::Account {
                expected: Some(account()),
            },
            1455,
            NOW,
        )
        .await
        .unwrap();
    assert!(matches!(
        auth.complete(tenant, start.id, &callback(&start), NOW + 600)
            .await,
        Err(AuthError::Transaction)
    ));
    let start = auth
        .begin(
            tenant,
            LoginPurpose::Account {
                expected: Some(account()),
            },
            1455,
            NOW,
        )
        .await
        .unwrap();
    let declined = callback(&start).replace("code=synthetic-code", "error=access_denied");
    assert!(matches!(
        auth.complete(tenant, start.id, &declined, NOW).await,
        Err(AuthError::Declined)
    ));
    assert!(matches!(
        auth.complete(tenant, start.id, &callback(&start), NOW)
            .await,
        Err(AuthError::Transaction)
    ));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    fake.next.lock().await["https://api.openai.com/auth"]["chatgpt_account_id"] =
        json!("wrong-workspace");
    let start = auth
        .begin(
            tenant,
            LoginPurpose::Account {
                expected: Some(account()),
            },
            1455,
            NOW,
        )
        .await
        .unwrap();
    assert!(matches!(
        auth.complete(tenant, start.id, &callback(&start), NOW)
            .await,
        Err(AuthError::Identity)
    ));
    assert!(
        vault
            .read(tenant, &SecretScope::Account(account()))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn refresh_is_coalesced_through_commit_and_rejects_changed_identity() {
    let (_, _, tenant, fake, auth) = setup().await;
    let initial = login(&auth, tenant).await;
    *fake.next.lock().await = token_claims(NOW + 7200);
    let account = account();
    let (a, b) = tokio::join!(
        auth.account(tenant, &account, Some(initial.access_token()), NOW),
        auth.account(tenant, &account, Some(initial.access_token()), NOW)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.access_token().expose(), b.access_token().expose());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2); // One initial exchange, one refresh.
    fake.next.lock().await["https://api.openai.com/auth"]["user_id"] = json!("wrong-global-user");
    assert!(matches!(
        auth.account(tenant, &account, Some(a.access_token()), NOW)
            .await,
        Err(AuthError::SignInRequired)
    ));
    let current = auth.account(tenant, &account, None, NOW).await.unwrap();
    assert_eq!(current.access_token().expose(), a.access_token().expose());
}

#[tokio::test]
async fn transactions_survive_service_restart_but_not_consumption() {
    let (_, vault, tenant, fake, auth) = setup().await;
    let start = auth
        .begin(
            tenant,
            LoginPurpose::Account {
                expected: Some(account()),
            },
            1455,
            NOW,
        )
        .await
        .unwrap();
    drop(auth);
    let restarted = AuthService::new(vault, fake);
    assert!(
        restarted
            .complete(tenant, start.id, &callback(&start), NOW)
            .await
            .is_ok()
    );
    assert!(matches!(
        restarted
            .complete(tenant, start.id, &callback(&start), NOW)
            .await,
        Err(AuthError::Transaction)
    ));
}

#[test]
fn step_up_requires_both_fresh_timestamps_exact_scope_and_membership() {
    let account = AccountTokens::validate(response(token_claims(NOW + 3600)), None, NOW).unwrap();
    let now =
        time::OffsetDateTime::from_unix_timestamp(NOW).unwrap() + time::Duration::milliseconds(500);
    let mut c = token_claims(NOW + 3600);
    c["iat"] = json!(NOW);
    c["pwd_auth_time"] = json!(NOW * 1000);
    c["scp"] = json!(["codex.remote_control.enroll"]);
    assert!(validate_step_up(&token(c.clone()), &account, "member-example", now).is_ok());
    assert!(validate_step_up(&token(c.clone()), &account, "global-example", now).is_ok());
    let mut missing_member = c.clone();
    missing_member["https://api.openai.com/auth"]
        .as_object_mut()
        .unwrap()
        .remove("chatgpt_account_user_id");
    assert!(
        validate_step_up(
            &token(missing_member.clone()),
            &account,
            "global-example",
            now
        )
        .is_err()
    );
    missing_member["https://api.openai.com/auth"]["account_user_id"] = json!("member-example");
    assert!(
        validate_step_up(
            &token(missing_member.clone()),
            &account,
            "global-example",
            now
        )
        .is_ok()
    );
    missing_member["https://api.openai.com/auth"]["account_user_id"] = json!("global-example");
    assert!(validate_step_up(&token(missing_member), &account, "global-example", now).is_err());
    for (iat, pwd) in [(NOW, NOW * 1000 + 500), (NOW + 1, NOW * 1000 + 501)] {
        let mut fresh = c.clone();
        fresh["iat"] = json!(iat);
        fresh["pwd_auth_time"] = json!(pwd);
        assert!(validate_step_up(&token(fresh), &account, "member-example", now).is_ok());
    }
    let mut boundary = c.clone();
    boundary["pwd_auth_time"] = json!(NOW * 1000 - 299500);
    assert!(validate_step_up(&token(boundary.clone()), &account, "member-example", now).is_ok());
    boundary["pwd_auth_time"] = json!(NOW * 1000 - 299501);
    assert!(validate_step_up(&token(boundary), &account, "member-example", now).is_err());
    for (field, value) in [
        ("iat", json!(NOW - 301)),
        ("pwd_auth_time", json!(NOW)),
        ("pwd_auth_time", json!(NOW * 1000 - 300001)),
        ("scope", json!("extra.permission")),
        ("scp", json!(["codex.remote_control.enroll", 17])),
        ("iat", json!("not a number")),
    ] {
        let mut bad = c.clone();
        bad[field] = value;
        assert!(
            validate_step_up(&token(bad), &account, "member-example", now).is_err(),
            "field {field}"
        );
    }
    for field in ["chatgpt_account_id", "chatgpt_account_user_id", "user_id"] {
        let mut bad = c.clone();
        bad["https://api.openai.com/auth"][field] = json!("other");
        assert!(validate_step_up(&token(bad), &account, "member-example", now).is_err());
    }
}

#[tokio::test]
async fn disk_reopen_keep_secrets_encrypted_and_identity_stable() {
    let path = std::env::temp_dir().join(format!("bridge-vault-test-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let old = sqlx::SqlitePool::connect_with(options.clone())
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../src/schema.sql"))
        .execute(&old)
        .await
        .unwrap();
    let tenant = Uuid::from_u128(1);
    sqlx::query("INSERT INTO tenants(id) VALUES(?)")
        .bind(tenant.to_string())
        .execute(&old)
        .await
        .unwrap();
    old.close().await;
    let store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[11; 32]).unwrap());
    let seed = vault.tenant_seed(tenant).await.unwrap();
    let endpoint = identity::endpoint(&seed, tenant, account(), "desk", EndpointKind::Out).unwrap();
    let auth = AuthService::new(vault.clone(), FakeToken::new());
    let credentials = login(&auth, tenant).await;
    store.create_endpoint(&endpoint).await.unwrap();
    let pending = auth
        .begin(
            tenant,
            LoginPurpose::Account {
                expected: Some(account()),
            },
            1455,
            NOW,
        )
        .await
        .unwrap();
    let state = url::Url::parse(&pending.url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    drop(auth);
    drop(vault);
    Arc::try_unwrap(store).ok().unwrap().close().await;
    let disk = std::fs::read(&path).unwrap();
    for secret in [
        credentials.access_token().expose().as_bytes(),
        b"synthetic-refresh",
        state.as_bytes(),
    ] {
        assert!(!disk.windows(secret.len()).any(|w| w == secret));
    }
    let store = Arc::new(SqliteStore::open(options).await.unwrap());
    let wrong_key = Vault::new(store.clone(), EncryptionKey::from_bytes(&[12; 32]).unwrap());
    assert!(wrong_key.tenant_seed(tenant).await.is_err());
    drop(wrong_key);
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[11; 32]).unwrap());
    let seed = vault.tenant_seed(tenant).await.unwrap();
    assert_eq!(
        endpoint.device_id,
        identity::endpoint(&seed, tenant, account(), "desk", EndpointKind::Out)
            .unwrap()
            .device_id
    );
    let auth = AuthService::new(vault.clone(), FakeToken::new());
    assert!(
        auth.complete(tenant, pending.id, &callback(&pending), NOW)
            .await
            .is_ok()
    );
    drop(auth);
    drop(vault);
    Arc::try_unwrap(store).ok().unwrap().close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn cached_account_labels_use_existing_credentials_without_refresh_or_cross_tenant_reads() {
    let (store, vault, tenant, fake, auth) = setup().await;
    let mut claims = token_claims(NOW + 3600);
    claims["https://api.openai.com/profile"] =
        json!({"email":" person@example.test ","name":"Example Person"});
    *fake.next.lock().await = claims;
    let tokens = login(&auth, tenant).await;
    let calls = fake.calls.load(Ordering::SeqCst);
    // An old persisted record has no extra profile fields and may be expired.
    let mut stored = serde_json::to_value(&tokens).unwrap();
    stored["expires_at"] = json!(0);
    vault
        .write(
            tenant,
            &SecretScope::Account(account()),
            &serde_json::to_vec(&stored).unwrap(),
        )
        .await
        .unwrap();
    let profile = auth.cached_profile(tenant, &account()).await.unwrap();
    assert_eq!(profile.email.as_deref(), Some("person@example.test"));
    assert_eq!(profile.name.as_deref(), Some("Example Person"));
    let other = Uuid::from_u128(2);
    store.create_tenant(other).await.unwrap();
    assert!(
        auth.cached_profile(other, &account())
            .await
            .unwrap()
            .email
            .is_none()
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), calls);
    // Missing/malformed optional display fields do not invalidate account authorization.
    for profile in [
        json!(null),
        json!({"email":42,"name":"bad\nname"}),
        json!({"email":"x".repeat(255),"name":[]}),
    ] {
        let mut claims = token_claims(NOW + 3600);
        claims["https://api.openai.com/profile"] = profile;
        let tokens = AccountTokens::validate(response(claims), Some(&account()), NOW).unwrap();
        vault
            .write(
                tenant,
                &SecretScope::Account(account()),
                &serde_json::to_vec(&tokens).unwrap(),
            )
            .await
            .unwrap();
        let profile = auth.cached_profile(tenant, &account()).await.unwrap();
        assert!(profile.email.is_none());
        assert!(profile.name.is_none());
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn sign_in_failure_is_durable_and_only_account_login_clears_it() {
    let path = std::env::temp_dir().join(format!("bridge-sign-in-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
    let tenant = Uuid::new_v4();
    store.create_tenant(tenant).await.unwrap();
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[11; 32]).unwrap());
    let fake = FakeToken::new();
    let auth = AuthService::new(vault.clone(), fake.clone());
    let tokens = login(&auth, tenant).await;
    *fake.failure.lock().await = Some(AuthError::Network);
    assert!(matches!(
        auth.account(tenant, &account(), Some(tokens.access_token()), NOW)
            .await,
        Err(AuthError::Network)
    ));
    assert!(!store.sign_in_required(tenant, &account()).await.unwrap());
    *fake.failure.lock().await = Some(AuthError::Rejected);
    assert!(matches!(
        auth.account(tenant, &account(), Some(tokens.access_token()), NOW)
            .await,
        Err(AuthError::SignInRequired)
    ));
    assert!(store.sign_in_required(tenant, &account()).await.unwrap());
    drop(auth);
    drop(vault);
    Arc::try_unwrap(store).ok().unwrap().close().await;
    let store = Arc::new(SqliteStore::open(options).await.unwrap());
    assert!(store.sign_in_required(tenant, &account()).await.unwrap());
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[11; 32]).unwrap());
    let auth = AuthService::new(vault, fake.clone());
    login(&auth, tenant).await;
    assert!(!store.sign_in_required(tenant, &account()).await.unwrap());
    fake.next.lock().await["https://api.openai.com/auth"]["chatgpt_account_id"] =
        json!("wrong-workspace");
    let current = auth.account(tenant, &account(), None, NOW).await.unwrap();
    assert!(matches!(
        auth.account(tenant, &account(), Some(current.access_token()), NOW)
            .await,
        Err(AuthError::SignInRequired)
    ));
    assert!(store.sign_in_required(tenant, &account()).await.unwrap());
    drop(auth);
    Arc::try_unwrap(store).ok().unwrap().close().await;
    std::fs::remove_file(path).unwrap();
}
