use axum::{Json, Router, routing::get};
use bridge_core::{
    auth::AuthService,
    chains::RuntimeFactory,
    management::ManagementAuth,
    storage::SqliteStore,
    upstream::Upstream,
    vault::{SecretScope, Vault},
};
use bridge_server::{api, key_file, web};
use sqlx::sqlite::SqliteConnectOptions;
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_target(false).init();
    let bind: SocketAddr = std::env::var("BRIDGE_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()?;
    let stop = CancellationToken::new();
    let mut database_lock = None;
    let mut api_state = None;
    let (app, listener) = match (
        std::env::var_os("BRIDGE_DATABASE"),
        std::env::var_os("BRIDGE_MASTER_KEY"),
    ) {
        (None, None) => {
            if !bind.ip().is_loopback() {
                return Err("未配置管理认证时，只允许监听回环地址".into());
            }
            let app = Router::new().route(
                "/api/capabilities",
                get(|| async { Json(bridge_core::capabilities()) }),
            );
            (app, tokio::net::TcpListener::bind(bind).await?)
        }
        (Some(database), Some(key)) => {
            let database = PathBuf::from(database);
            let key = PathBuf::from(key);
            let parent = |path: &std::path::Path| {
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(std::path::Path::new("."))
                    .canonicalize()
            };
            if parent(&key)?.starts_with(parent(&database)?) {
                return Err("请将主密钥放在数据库目录之外，并分别备份".into());
            }
            database_lock = Some(key_file::lock_database(&database)?);
            let kek = key_file::load(&key)?;
            let store =
                Arc::new(SqliteStore::open(SqliteConnectOptions::new().filename(database)).await?);
            let vault = Vault::new(store.clone(), kek);
            let management = Arc::new(ManagementAuth::new(store.clone()));
            if !management.has_users().await? {
                return Err("请先使用 bridge-admin add-user 创建管理登录".into());
            }
            let tenants = store.local_tenants().await?;
            for tenant in &tenants {
                let seed = vault
                    .read(*tenant, &SecretScope::TenantSeed)
                    .await?
                    .ok_or("租户种子缺失，不能重新生成身份")?;
                if seed.len() != 32 {
                    return Err("租户种子无效".into());
                }
            }
            let origin = std::env::var("BRIDGE_PUBLIC_ORIGIN")
                .map_err(|_| "请配置 BRIDGE_PUBLIC_ORIGIN 为浏览器访问的固定入口")?;
            api::ApiState::new(management.clone(), HashMap::new(), &origin)?;
            let ui = std::env::var_os("BRIDGE_UI_DIR").map(PathBuf::from);
            if ui
                .as_ref()
                .is_some_and(|dir| !dir.join("index.html").is_file())
            {
                return Err("BRIDGE_UI_DIR 中没有 index.html，请指向构建好的网页目录".into());
            }
            let proxy = bridge_core::deployment::bootstrap(
                &vault,
                std::env::var("BRIDGE_UPSTREAM_PROXY").ok(),
            )
            .await?;
            let upstream =
                Upstream::new(proxy.as_ref().map(bridge_core::auth::SecretString::expose))?;
            let auth = Arc::new(AuthService::new(vault.clone(), upstream.clone()));
            // Bind successfully before starting remote work, including pending deletion cleanup.
            let listener = tokio::net::TcpListener::bind(bind).await?;
            let factory = RuntimeFactory {
                store: store.clone(),
                vault: vault.clone(),
                auth,
                upstream: upstream.clone(),
            };
            let state = api::ApiState::new(management, HashMap::new(), &origin)
                .expect("origin was validated before spawning workers")
                .with_network(vault, upstream)
                .with_runtime(factory, stop.clone());
            for tenant in tenants {
                state.register_tenant(tenant).await?;
            }
            api_state = Some(state.clone());
            // The built UI is served here so any reverse proxy can forward the whole site.
            let app = match ui {
                Some(dir) => api::router(state).merge(web::router(dir)),
                None => api::router(state),
            };
            (app, listener)
        }
        _ => return Err("BRIDGE_DATABASE 和 BRIDGE_MASTER_KEY 必须同时配置".into()),
    };
    tracing::info!(event = "server.listening", address = %listener.local_addr()?);
    let shutdown = stop.clone();
    let serving = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .into_future();
    tokio::pin!(serving);
    let result = tokio::select! {
        result = &mut serving => result,
        _ = shutdown_signal() => {
            stop.cancel();
            serving.await
        }
    };
    stop.cancel();
    if let Some(state) = api_state {
        state.finish_operations().await;
        if let Err(error) = state.shutdown_spaces().await {
            tracing::error!(event = "server.coordinator_shutdown_failed");
            return Err(error.into());
        }
    }
    drop(database_lock);
    result?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = terminate.recv() => {},
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
