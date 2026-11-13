use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};
use bridge_core::{
    identity::{self, TenantSeed},
    model::{AccountIdentity, Binding, EndpointKind},
    storage::SqliteStore,
};
use sqlx::sqlite::SqliteConnectOptions;
use uuid::Uuid;
fn account() -> AccountIdentity {
    AccountIdentity {
        workspace_id: "workspace-example".into(),
        user_id: "user-example".into(),
    }
}
fn tenant() -> Uuid {
    Uuid::from_u128(1)
}

#[test]
fn stable_identity_and_signatures() {
    let seed = TenantSeed::from_bytes([7; 32]);
    let input = identity::endpoint(&seed, tenant(), account(), "desk", EndpointKind::In).unwrap();
    let output = identity::endpoint(&seed, tenant(), account(), "desk", EndpointKind::Out).unwrap();
    assert_ne!(input.device_id, output.device_id);
    assert_eq!(
        output,
        identity::endpoint(&seed, tenant(), account(), "desk", EndpointKind::Out).unwrap()
    );
    assert_ne!(
        output.device_id,
        identity::endpoint(
            &seed,
            Uuid::from_u128(2),
            account(),
            "desk",
            EndpointKind::Out
        )
        .unwrap()
        .device_id
    );
    let key = identity::signing_key(&seed, tenant(), &account(), "desk").unwrap();
    assert_eq!(
        key.public_key(),
        identity::signing_key(&seed, tenant(), &account(), "desk")
            .unwrap()
            .public_key()
    );
    let other = identity::signing_key(&seed, tenant(), &account(), "Desk").unwrap();
    assert_ne!(key.public_key(), other.public_key());
    let signature = key.sign(b"sample challenge").unwrap();
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key.public_key())
        .verify(b"sample challenge", &signature)
        .unwrap();
    assert!(
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key.public_key())
            .verify(b"changed", &signature)
            .is_err()
    );
    for invalid in ["", " desk", "desk ", "de\nsk"] {
        assert!(identity::endpoint(&seed, tenant(), account(), invalid, EndpointKind::In).is_err());
    }
}

#[tokio::test]
async fn storage_enforces_tenants_roles_and_exclusivity() {
    let store = SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    let a = tenant();
    let b = Uuid::from_u128(2);
    let seed = TenantSeed::from_bytes([7; 32]);
    for tenant in [a, b] {
        store.create_tenant(tenant).await.unwrap();
        store.create_account(tenant, &account()).await.unwrap();
    }
    let input = identity::endpoint(&seed, a, account(), "phone", EndpointKind::In).unwrap();
    let input2 = identity::endpoint(&seed, a, account(), "tablet", EndpointKind::In).unwrap();
    let output = identity::endpoint(&seed, a, account(), "desk", EndpointKind::Out).unwrap();
    let foreign = identity::endpoint(&seed, b, account(), "desk", EndpointKind::Out).unwrap();
    for endpoint in [&input, &input2, &output, &foreign] {
        store.create_endpoint(endpoint).await.unwrap();
    }
    assert_eq!(
        store.endpoint(a, input.device_id).await.unwrap(),
        Some(input.clone())
    );
    assert!(store.endpoint(b, input.device_id).await.unwrap().is_none());
    assert!(!store.delete_draft(b, input.device_id).await.unwrap());
    assert!(
        store
            .bind(&Binding {
                tenant_id: a,
                in_id: input.device_id,
                out_id: foreign.device_id
            })
            .await
            .is_err()
    );
    assert!(
        store
            .bind(&Binding {
                tenant_id: a,
                in_id: input.device_id,
                out_id: input2.device_id
            })
            .await
            .is_err()
    );
    let first = Binding {
        tenant_id: a,
        in_id: input.device_id,
        out_id: output.device_id,
    };
    let second = Binding {
        tenant_id: a,
        in_id: input2.device_id,
        out_id: output.device_id,
    };
    let (x, y) = tokio::join!(store.bind(&first), store.bind(&second));
    assert_ne!(x.is_ok(), y.is_ok());
    assert!(store.delete_draft(a, output.device_id).await.is_err());
    assert!(!store.unbind(b, input.device_id).await.unwrap());
    store
        .unbind(
            a,
            if x.is_ok() {
                input.device_id
            } else {
                input2.device_id
            },
        )
        .await
        .unwrap();
    assert!(store.delete_draft(a, output.device_id).await.unwrap());
    store.create_endpoint(&output).await.unwrap();
    assert_eq!(
        store.endpoint(a, output.device_id).await.unwrap(),
        Some(output)
    );
}

#[test]
fn frozen_vector_matches_independent_webcrypto_and_openssl() {
    let vector: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/identity.json")).unwrap();
    let seed = TenantSeed::from_bytes([7; 32]);
    for (kind, field) in [(EndpointKind::In, "in_id"), (EndpointKind::Out, "out_id")] {
        assert_eq!(
            identity::endpoint(&seed, tenant(), account(), "desk", kind)
                .unwrap()
                .device_id
                .to_string(),
            vector[field].as_str().unwrap()
        );
    }
    let key = identity::signing_key(&seed, tenant(), &account(), "desk").unwrap();
    let hex: String = key
        .public_key()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(hex, vector["public_key_hex"].as_str().unwrap());
}

#[tokio::test]
async fn metadata_survives_reopen() {
    let path = std::env::temp_dir().join(format!("bridge-foundation-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let store = SqliteStore::open(options.clone()).await.unwrap();
    let endpoint = identity::endpoint(
        &TenantSeed::from_bytes([7; 32]),
        tenant(),
        account(),
        "desk",
        EndpointKind::Out,
    )
    .unwrap();
    store.create_tenant(tenant()).await.unwrap();
    store.create_account(tenant(), &account()).await.unwrap();
    store.create_endpoint(&endpoint).await.unwrap();
    store.close().await;
    let reopened = SqliteStore::open(options).await.unwrap();
    assert_eq!(
        reopened
            .endpoint(tenant(), endpoint.device_id)
            .await
            .unwrap(),
        Some(endpoint)
    );
    reopened.close().await;
    std::fs::remove_file(path).unwrap();
}
