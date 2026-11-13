//! Deployment-wide configuration. No upstream secrets are returned to the browser.
use super::*;
use bridge_core::{deployment, upstream::Upstream, vault::Vault};
use tokio::sync::RwLock;

#[derive(Clone)]
pub(super) struct NetworkSettings {
    pub vault: Vault,
    pub upstream: Upstream,
}
pub(super) fn admin(session: &SessionIdentity) -> Result<(), ApiError> {
    if session.deployment_admin {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "deployment_admin_required",
        })
    }
}
fn view(settings: &NetworkSettings, configured: bool) -> serde_json::Value {
    let proxy = settings.upstream.proxy_url();
    let mut address = None;
    let mut authenticated = false;
    if let Some(raw) = proxy
        && let Ok(mut url) = url::Url::parse(&raw)
    {
        authenticated = !url.username().is_empty() || url.password().is_some();
        let _ = url.set_username("");
        let _ = url.set_password(None);
        address = Some(url.to_string());
    }
    json!({"configured":configured,"mode":if address.is_some(){"proxy"}else{"direct"},"address":address,"authenticated":authenticated})
}
pub(super) async fn get_settings(
    State(state): State<ApiState>,
    Extension(session): Extension<SessionIdentity>,
) -> Result<Json<serde_json::Value>, ApiError> {
    admin(&session)?;
    let settings = state.network.as_ref().ok_or(ApiError::unavailable())?;
    let configured = deployment::load(&settings.vault)
        .await
        .map_err(|_| ApiError::internal())?
        .is_some();
    Ok(Json(view(settings, configured)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Update {
    #[serde(deserialize_with = "Option::deserialize")]
    proxy_url: Option<SecretString>,
}
pub(super) async fn update(
    State(state): State<ApiState>,
    Extension(session): Extension<SessionIdentity>,
    ApiJson(body): ApiJson<Update>,
) -> Result<Json<serde_json::Value>, ApiError> {
    admin(&session)?;
    let settings = state.network.clone().ok_or(ApiError::unavailable())?;
    let prepared =
        Upstream::new(body.proxy_url.as_ref().map(SecretString::expose)).map_err(|_| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_proxy",
        })?;
    // Own the operation after acceptance, even if the browser disconnects.
    let tasks = state.operations.clone();
    tasks
        .spawn(async move {
            let _gate = state.network_gate.write().await;
            let controllers = state.registry.controllers();
            let mut paused = true;
            for controller in &controllers {
                if controller.pause_network().await.is_err() {
                    paused = false;
                }
            }
            if !paused {
                for controller in &controllers {
                    let _ = controller.resume_network().await;
                }
                return Err(ApiError::unavailable());
            }
            if deployment::save(&settings.vault, body.proxy_url.as_ref())
                .await
                .is_err()
            {
                for controller in &controllers {
                    let _ = controller.resume_network().await;
                }
                return Err(ApiError::internal());
            }
            settings.upstream.replace_network(&prepared);
            let mut failures = 0;
            for controller in &controllers {
                if controller.resume_network().await.is_err() {
                    failures += 1;
                }
            }
            let mut result = view(&settings, true);
            result["restart_failures"] = json!(failures);
            Ok(Json(result))
        })
        .await
        .map_err(|_| ApiError::internal())?
}
impl ApiState {
    pub fn with_network(mut self, vault: Vault, upstream: Upstream) -> Self {
        self.network = Some(NetworkSettings { vault, upstream });
        self
    }
    pub async fn finish_operations(&self) {
        self.operations.close();
        self.operations.wait().await;
    }
}
pub(super) fn gate() -> Arc<RwLock<()>> {
    Arc::new(RwLock::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use bridge_core::{auth::SecretString, storage::SqliteStore, vault::EncryptionKey};
    use sqlx::sqlite::SqliteConnectOptions;
    use tower::ServiceExt;
    async fn request(
        app: Router,
        token: &str,
        method: Method,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/deployment/network")
                    .header("host", "localhost:8787")
                    .header("origin", "http://localhost:8787")
                    .header("cookie", format!("bridge_session={token}"))
                    .header("x-csrf-token", csrf_token(token).unwrap())
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 16384).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }
    #[tokio::test]
    async fn proxy_is_admin_only_encrypted_persistent_and_applied_to_all_clones() {
        let store = Arc::new(
            SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
                .await
                .unwrap(),
        );
        let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[27; 32]).unwrap());
        let auth = Arc::new(ManagementAuth::new(store.clone()));
        let mut tokens = Vec::new();
        for name in ["administrator", "tenant"] {
            let tenant = Uuid::new_v4();
            store.create_tenant(tenant).await.unwrap();
            auth.create_user(tenant, name, SecretString::new("synthetic-password".into()))
                .await
                .unwrap();
            let login = auth
                .login(
                    name,
                    SecretString::new("synthetic-password".into()),
                    unix_time(),
                )
                .await
                .unwrap();
            assert_eq!(login.identity.deployment_admin, name == "administrator");
            tokens.push(login.token);
        }
        let upstream = Upstream::new(None).unwrap();
        let clone = upstream.clone();
        let state = ApiState::new(auth, HashMap::new(), "http://localhost:8787")
            .unwrap()
            .with_network(vault.clone(), upstream);
        let app = router(state.clone());
        let admin = tokens[0].expose();
        let tenant = tokens[1].expose();
        for method in [Method::GET, Method::POST] {
            let (status, body) =
                request(app.clone(), tenant, method, json!({"proxy_url":null})).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body["error"], "deployment_admin_required");
        }
        let proxy = "http://demo:synthetic-proxy-password@127.0.0.1:9";
        let (status, body) =
            request(app.clone(), admin, Method::POST, json!({"proxy_url":proxy})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["authenticated"], true);
        assert!(!body.to_string().contains("synthetic-proxy-password"));
        assert!(!body.to_string().contains("demo:"));
        assert_eq!(clone.proxy_url().as_deref(), Some(proxy));
        assert_eq!(
            deployment::load(&vault)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .expose(),
            proxy
        );
        // Saved configuration wins over stale process environment on subsequent starts.
        assert_eq!(
            deployment::bootstrap(&vault, Some("http://127.0.0.1:3".into()))
                .await
                .unwrap()
                .unwrap()
                .expose(),
            proxy
        );
        let (status, _) = request(
            app.clone(),
            admin,
            Method::POST,
            json!({"proxy_url":"socks5://127.0.0.1:9"}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(clone.proxy_url().as_deref(), Some(proxy));
        let (status, body) =
            request(app.clone(), admin, Method::POST, json!({"proxy_url":null})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["mode"], "direct");
        assert!(clone.proxy_url().is_none());
        assert!(
            deployment::bootstrap(&vault, Some(proxy.into()))
                .await
                .unwrap()
                .is_none()
        );
        // A browser disconnect must not cancel an already accepted settings operation.
        let lease = state.network_gate.read().await;
        let token = admin.to_owned();
        let applying = tokio::spawn(async move {
            request(app, &token, Method::POST, json!({"proxy_url":proxy})).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while state.operations.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        applying.abort();
        let _ = applying.await;
        drop(lease);
        state.finish_operations().await;
        assert_eq!(clone.proxy_url().as_deref(), Some(proxy));
        assert_eq!(
            deployment::load(&vault)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .expose(),
            proxy
        );
    }
}
