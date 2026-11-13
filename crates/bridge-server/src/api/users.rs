use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewUser {
    username: String,
    password: SecretString,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Password {
    password: SecretString,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnPassword {
    current: SecretString,
    password: SecretString,
}

pub(super) async fn list(
    State(state): State<ApiState>,
    Extension(session): Extension<SessionIdentity>,
) -> Result<Json<serde_json::Value>, ApiError> {
    network::admin(&session)?;
    Ok(Json(
        json!({"users":state.auth.users(session.user_id, unix_time()).await?}),
    ))
}
pub(super) async fn add(
    State(state): State<ApiState>,
    Extension(session): Extension<SessionIdentity>,
    ApiJson(body): ApiJson<NewUser>,
) -> Result<Response, ApiError> {
    network::admin(&session)?;
    let factory = state.runtime.clone().ok_or(ApiError::unavailable())?;
    state.operations.clone().spawn(async move {
        let _gate = state.network_gate.write().await;
        let (id, tenant) = state.auth.add_web_user(&factory.vault, &body.username, body.password).await?;
        state.registry.add(tenant, factory, &state.stop);
        Ok((StatusCode::CREATED, Json(json!({"id":id,"username":body.username,"deployment_admin":false,"locked":false,"current":false}))).into_response())
    }).await.map_err(|_| ApiError::internal())?
}
pub(super) async fn reset(
    State(state): State<ApiState>,
    Extension(session): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<Password>,
) -> Result<StatusCode, ApiError> {
    network::admin(&session)?;
    state.auth.reset_web_password(id, body.password).await?;
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn remove(
    State(state): State<ApiState>,
    Extension(session): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    network::admin(&session)?;
    state
        .operations
        .clone()
        .spawn(async move {
            let _gate = state.network_gate.write().await;
            let tenant = state.auth.non_admin_tenant(id).await?;
            state.registry.remove(tenant).await?;
            if let Err(error) = state.auth.delete_web_user(id).await {
                let factory = state.runtime.clone().ok_or(ApiError::internal())?;
                state.registry.add(tenant, factory, &state.stop);
                return Err(error.into());
            }
            Ok(StatusCode::NO_CONTENT)
        })
        .await
        .map_err(|_| ApiError::internal())?
}
pub(super) async fn change(
    State(state): State<ApiState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<OwnPassword>,
) -> Result<StatusCode, ApiError> {
    state
        .auth
        .change_password(
            cookie_token(&headers)?,
            body.current,
            body.password,
            unix_time(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use bridge_core::{
        auth::AuthService,
        chains::RuntimeFactory,
        storage::SqliteStore,
        upstream::Upstream,
        vault::{EncryptionKey, Vault},
    };
    use sqlx::sqlite::SqliteConnectOptions;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    pub(super) async fn fixture() -> (ApiState, String, SessionIdentity) {
        fixture_with_options(SqliteConnectOptions::new().in_memory(true)).await
    }
    async fn fixture_with_options(
        options: SqliteConnectOptions,
    ) -> (ApiState, String, SessionIdentity) {
        let store = Arc::new(SqliteStore::open(options).await.unwrap());
        let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[34; 32]).unwrap());
        let tenant = Uuid::new_v4();
        vault.create_tenant_with_seed(tenant).await.unwrap();
        let auth = Arc::new(ManagementAuth::new(store.clone()));
        auth.create_user(
            tenant,
            "administrator",
            SecretString::new("synthetic-password".into()),
        )
        .await
        .unwrap();
        let login = auth
            .login(
                "administrator",
                SecretString::new("synthetic-password".into()),
                unix_time(),
            )
            .await
            .unwrap();
        let upstream = Upstream::new(None).unwrap();
        let factory = RuntimeFactory {
            store,
            vault: vault.clone(),
            auth: Arc::new(AuthService::new(vault.clone(), upstream.clone())),
            upstream: upstream.clone(),
        };
        let state = ApiState::new(auth, HashMap::new(), "http://localhost:8787")
            .unwrap()
            .with_network(vault, upstream)
            .with_runtime(factory, CancellationToken::new());
        state.register_tenant(tenant).await.unwrap();
        (state, login.token.expose().to_owned(), login.identity)
    }
    pub(super) fn request(token: &str, method: &str, path: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost:8787")
            .header("origin", "http://localhost:8787")
            .header("cookie", format!("bridge_session={token}"))
            .header("x-csrf-token", csrf_token(token).unwrap())
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }
    #[tokio::test]
    async fn failed_delete_restores_state_and_event_subscription_without_restart() {
        use futures_util::StreamExt;
        let path = std::env::temp_dir().join(format!("bridge-delete-{}.sqlite", Uuid::new_v4()));
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let (state, token, identity) = fixture_with_options(options.clone()).await;
        let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
        let app = router(state.clone());
        let created = app
            .clone()
            .oneshot(request(
                &token,
                "POST",
                "/api/users",
                r#"{"username":"bob","password":"synthetic-password"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let id = state
            .auth
            .users(identity.user_id, unix_time())
            .await
            .unwrap()
            .into_iter()
            .find(|u| u.username == "bob")
            .unwrap()
            .id;
        let tenant = state.auth.non_admin_tenant(id).await.unwrap();
        let login = state
            .auth
            .login(
                "bob",
                SecretString::new("synthetic-password".into()),
                unix_time(),
            )
            .await
            .unwrap();
        let old_controller = state.registry.controller(tenant).unwrap();
        sqlx::query("CREATE TRIGGER fail_user_delete BEFORE DELETE ON management_users WHEN OLD.username='bob' BEGIN SELECT RAISE(ABORT,'synthetic delete failure'); END").execute(&pool).await.unwrap();
        let response = app
            .clone()
            .oneshot(request(&token, "DELETE", &format!("/api/users/{id}"), ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
            "internal_error"
        );
        assert!(old_controller.shutdown().is_cancelled());
        assert!(
            !state
                .registry
                .controller(tenant)
                .unwrap()
                .shutdown()
                .is_cancelled()
        );
        let response = app
            .clone()
            .oneshot(request(login.token.expose(), "GET", "/api/state", ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(request(login.token.expose(), "GET", "/api/events", ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut events = response.into_body().into_data_stream();
        let event = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            String::from_utf8(event.to_vec())
                .unwrap()
                .contains("event: changed")
        );
        sqlx::query("DROP TRIGGER fail_user_delete")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            app.oneshot(request(&token, "DELETE", &format!("/api/users/{id}"), ""))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        state.finish_operations().await;
        state.shutdown_spaces().await.unwrap();
        pool.close().await;
        std::fs::remove_file(path).unwrap();
    }
    #[tokio::test]
    async fn wrong_current_password_is_not_logout_and_lockout_uses_contract_code() {
        let (state, token, identity) = fixture().await;
        let app = router(state.clone());
        for _ in 0..5 {
            let response = app
                .clone()
                .oneshot(request(
                    &token,
                    "POST",
                    "/api/session/password",
                    r#"{"current":"wrong","password":"replacement-password"}"#,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["error"],
                "current_password_incorrect"
            );
        }
        let response = app
            .oneshot(request(
                &token,
                "POST",
                "/api/session/password",
                r#"{"current":"synthetic-password","password":"replacement-password"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["error"],
            "login_rate_limited"
        );
        assert!(state.auth.session(&token, unix_time()).await.is_ok());
        let users = state
            .auth
            .users(identity.user_id, unix_time())
            .await
            .unwrap();
        assert!(users[0].locked);
        state.finish_operations().await;
        state.shutdown_spaces().await.unwrap();
    }
    #[tokio::test]
    async fn accepted_add_and_delete_finish_after_http_cancellation_and_wait_for_network_gate() {
        let (state, token, identity) = fixture().await;
        let app = router(state.clone());
        for creating in [true, false] {
            let users = state
                .auth
                .users(identity.user_id, unix_time())
                .await
                .unwrap();
            let path = if creating {
                "/api/users".into()
            } else {
                format!(
                    "/api/users/{}",
                    users.iter().find(|u| u.username == "bob").unwrap().id
                )
            };
            let gate = state.network_gate.write().await;
            let req = request(
                &token,
                if creating { "POST" } else { "DELETE" },
                &path,
                r#"{"username":"bob","password":"synthetic-password"}"#,
            );
            let request = tokio::spawn(app.clone().oneshot(req));
            tokio::time::timeout(Duration::from_secs(2), async {
                while state.operations.is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                state
                    .auth
                    .users(identity.user_id, unix_time())
                    .await
                    .unwrap()
                    .len(),
                if creating { 1 } else { 2 }
            );
            request.abort();
            drop(gate);
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let users = state
                        .auth
                        .users(identity.user_id, unix_time())
                        .await
                        .unwrap();
                    if users.len() == if creating { 2 } else { 1 } && state.operations.is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        state.finish_operations().await;
        state.shutdown_spaces().await.unwrap();
    }
}

#[cfg(test)]
mod network_tests {
    use super::tests::{fixture, request};
    use super::*;
    use tower::ServiceExt;
    #[tokio::test]
    async fn proxy_switch_visits_new_space_and_removal_does_not_cancel_other_spaces() {
        let (state, token, identity) = fixture().await;
        let app = router(state.clone());
        let response = app
            .clone()
            .oneshot(request(
                &token,
                "POST",
                "/api/users",
                r#"{"username":"bob","password":"synthetic-password"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let users = state
            .auth
            .users(identity.user_id, unix_time())
            .await
            .unwrap();
        let id = users.iter().find(|u| u.username == "bob").unwrap().id;
        let tenant = state.auth.non_admin_tenant(id).await.unwrap();
        let controller = state.registry.controller(tenant).unwrap();
        let changes = controller.subscribe();
        let response = app
            .clone()
            .oneshot(request(
                &token,
                "POST",
                "/api/deployment/network",
                r#"{"proxy_url":null}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            changes.has_changed().unwrap(),
            "new space must actually receive pause/resume, not just be present in the registry"
        );
        let other = state.registry.controller(identity.tenant_id).unwrap();
        let response = app
            .oneshot(request(&token, "DELETE", &format!("/api/users/{id}"), ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(controller.shutdown().is_cancelled());
        assert!(!other.shutdown().is_cancelled());
        assert!(other.status().await.is_ok());
        state.finish_operations().await;
        state.shutdown_spaces().await.unwrap();
    }
}
