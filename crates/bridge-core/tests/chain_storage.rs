use bridge_core::{
    device::Registration,
    identity::{self, TenantSeed},
    model::{AccountIdentity, Binding, EndpointKind, Target},
    storage::SqliteStore,
    upstream::HostRegistration,
    vault::{EncryptionKey, Vault},
};
use sqlx::sqlite::SqliteConnectOptions;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn target_configuration_is_tenant_scoped_exclusive_and_rejects_local_in_cycles() {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let tenant = Uuid::from_u128(1);
    let foreign = Uuid::from_u128(2);
    let account = AccountIdentity {
        workspace_id: "workspace".into(),
        user_id: "user".into(),
    };
    for tenant in [tenant, foreign] {
        store.create_tenant(tenant).await.unwrap();
        store.create_account(tenant, &account).await.unwrap();
    }
    let seed = TenantSeed::from_bytes([7; 32]);
    let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[19; 32]).unwrap());
    let input =
        identity::endpoint(&seed, tenant, account.clone(), "input", EndpointKind::In).unwrap();
    let output =
        identity::endpoint(&seed, tenant, account.clone(), "output", EndpointKind::Out).unwrap();
    let foreign_input =
        identity::endpoint(&seed, foreign, account, "input", EndpointKind::In).unwrap();
    for endpoint in [&input, &output, &foreign_input] {
        store.create_endpoint(endpoint).await.unwrap();
    }
    let target = Target {
        environment_id: "real-b".into(),
        app_server_version: "mock/1".into(),
    };
    assert!(
        store
            .set_target(tenant, output.device_id, &target)
            .await
            .is_err()
    ); // Draft is not an authorized controller.
    vault
        .save_out_registration(
            tenant,
            output.device_id,
            &Registration {
                client_id: "controller".into(),
                account_user_id: "user".into(),
                key_id: "key".into(),
            },
        )
        .await
        .unwrap();
    vault
        .save_host_registration(
            foreign,
            foreign_input.device_id,
            &HostRegistration {
                server_id: "host".into(),
                environment_id: "local-in".into(),
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .set_target(foreign, output.device_id, &target)
            .await
            .is_err()
    );
    assert!(
        store
            .set_target(
                tenant,
                output.device_id,
                &Target {
                    environment_id: "local-in".into(),
                    ..target.clone()
                }
            )
            .await
            .is_err()
    );
    store
        .set_target(tenant, output.device_id, &target)
        .await
        .unwrap();
    assert!(
        store
            .target(foreign, output.device_id)
            .await
            .unwrap()
            .is_none()
    );
    store
        .bind(&Binding {
            tenant_id: tenant,
            in_id: input.device_id,
            out_id: output.device_id,
        })
        .await
        .unwrap();
    assert!(
        store
            .set_target(
                tenant,
                output.device_id,
                &Target {
                    environment_id: "another-b".into(),
                    ..target
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .target(tenant, output.device_id)
            .await
            .unwrap()
            .unwrap()
            .environment_id,
        "real-b"
    );
    assert_eq!(store.bindings(tenant).await.unwrap().len(), 1);
    assert!(store.bindings(foreign).await.unwrap().is_empty());
}
