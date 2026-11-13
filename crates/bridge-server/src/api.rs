//! Same-origin, authenticated management API. Tenant identity only comes from the session.
mod events;
mod network;
mod registry;
mod users;
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, FromRequest, Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use bridge_core::{
    auth::SecretString,
    chains::{ChainController, ChainError},
    configuration::{Command, Reply},
    management::{ManagementAuth, ManagementError, SESSION_SECONDS, SessionIdentity, csrf_token},
    model::{AccountIdentity, EndpointKind},
    operation_error::{self, OperationError},
    upstream::unix_time,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Semaphore;
use uuid::Uuid;

pub use registry::InitialSpaces;

const COOKIE: &str = "bridge_session";
#[derive(Clone)]
pub struct ApiState {
    network: Option<network::NetworkSettings>,
    operations: tokio_util::task::TaskTracker,
    network_gate: Arc<tokio::sync::RwLock<()>>,
    auth: Arc<ManagementAuth>,
    registry: Arc<registry::Registry>,
    runtime: Option<bridge_core::chains::RuntimeFactory>,
    stop: tokio_util::sync::CancellationToken,
    origin: String,
    authority: String,
    secure: bool,
    requests: Arc<Semaphore>,
    streams: Arc<Semaphore>,
}
impl ApiState {
    pub fn new(
        auth: Arc<ManagementAuth>,
        chains: InitialSpaces,
        origin: &str,
    ) -> Result<Self, &'static str> {
        let url = url::Url::parse(origin).map_err(|_| "管理入口地址无效")?;
        let loopback = match url.host() {
            Some(url::Host::Domain("localhost")) => true,
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if (!matches!(url.scheme(), "http" | "https"))
            || (url.scheme() == "http" && !loopback)
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
        {
            return Err(
                "管理入口须为 HTTPS 地址；仅回环开发地址允许 HTTP，不带路径、查询或用户信息",
            );
        }
        let normalized = url.origin().ascii_serialization();
        let authority = normalized
            .split_once("://")
            .ok_or("管理入口地址无效")?
            .1
            .to_owned();
        Ok(Self {
            network: None,
            operations: tokio_util::task::TaskTracker::new(),
            network_gate: network::gate(),
            auth,
            registry: Arc::new(registry::Registry::new(chains)),
            runtime: None,
            stop: tokio_util::sync::CancellationToken::new(),
            origin: normalized,
            authority,
            secure: url.scheme() == "https",
            requests: Arc::new(Semaphore::new(128)),
            streams: Arc::new(Semaphore::new(64)),
        })
    }
    fn controller(&self, session: &SessionIdentity) -> Result<ChainController, ApiError> {
        self.registry.controller(session.tenant_id)
    }
    pub fn with_runtime(
        mut self,
        factory: bridge_core::chains::RuntimeFactory,
        stop: tokio_util::sync::CancellationToken,
    ) -> Self {
        self.runtime = Some(factory);
        self.stop = stop;
        self
    }
    pub async fn register_tenant(&self, tenant: Uuid) -> Result<(), &'static str> {
        let _gate = self.network_gate.write().await;
        self.registry.add(
            tenant,
            self.runtime.clone().ok_or("runtime is not configured")?,
            &self.stop,
        );
        Ok(())
    }
    pub async fn shutdown_spaces(&self) -> Result<(), &'static str> {
        self.registry
            .shutdown()
            .await
            .map_err(|_| "部分协调器未正常退出")
    }
    fn cookie(&self, token: &str, age: i64) -> Result<HeaderValue, ApiError> {
        HeaderValue::from_str(&format!(
            "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={age}{}",
            if self.secure { "; Secure" } else { "" }
        ))
        .map_err(|_| ApiError::internal())
    }
}
pub fn router(state: ApiState) -> Router {
    let protected = Router::new()
        .route("/api/session", get(session).delete(logout))
        .route("/api/session/password", post(users::change))
        .route("/api/users", get(users::list).post(users::add))
        .route("/api/users/{id}", delete(users::remove))
        .route("/api/users/{id}/password", post(users::reset))
        .route("/api/state", get(snapshot))
        .route(
            "/api/deployment/network",
            get(network::get_settings).post(network::update),
        )
        .route("/api/events", get(events::subscribe))
        .route("/api/accounts/login", post(begin_account))
        .route("/api/accounts/check", post(check_account))
        .route(
            "/api/logins/{id}",
            post(complete_login).delete(cancel_login),
        )
        .route("/api/endpoints", post(create_endpoint))
        .route("/api/endpoints/{id}", delete(delete_endpoint))
        .route("/api/outputs/{id}/authorize", post(begin_out))
        .route("/api/outputs/{id}/recover", post(recover_out))
        .route("/api/outputs/{id}/restore-record", post(restore_out_record))
        .route("/api/outputs/{id}/pair", post(pair_out))
        .route("/api/outputs/{id}/device/sync", post(sync_output_device))
        .route("/api/inputs/{id}/pair", post(pair_in))
        .route("/api/inputs/{id}/device/sync", post(sync_input_device))
        .route("/api/inputs/{id}/binding", post(bind).delete(unbind))
        .route("/api/inputs/{id}/start", post(start))
        .route("/api/inputs/{id}/stop", post(stop))
        .route("/api/inputs/{id}/switch", post(switch))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new().merge(protected)
        .route("/api/session",post(login))
        .route("/api/capabilities",get(||async{Json(json!({"version":env!("CARGO_PKG_VERSION"),"remoteControl":true,"management":true}))}))
        .layer(DefaultBodyLimit::max(32*1024))
        .layer(middleware::from_fn_with_state(state.clone(),boundary))
        .with_state(state)
}
async fn boundary(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let response = async {
        let _permit = state
            .requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::unavailable())?;
        if single(request.headers(), header::HOST.as_str()) != Some(state.authority.as_str()) {
            return Err(ApiError::forbidden());
        }
        let origin = single(request.headers(), header::ORIGIN.as_str());
        if (request.headers().contains_key(header::ORIGIN) && origin.is_none())
            || origin.is_some_and(|value| value != state.origin)
            || (!matches!(*request.method(), Method::GET | Method::HEAD)
                && origin != Some(state.origin.as_str()))
        {
            return Err(ApiError::forbidden());
        }
        // Never infer a tenant, trusted origin, or TLS policy from forwarded headers.
        Ok(next.run(request).await)
    }
    .await;
    let mut response = response.unwrap_or_else(IntoResponse::into_response);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    response
}
fn single<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(value)
}
fn cookie_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    let raw = single(headers, header::COOKIE.as_str()).ok_or(ApiError::unauthorized())?;
    let mut token = None;
    for part in raw.split(';') {
        if let Some((name, value)) = part.trim().split_once('=')
            && name == COOKIE
            && token.replace(value).is_some()
        {
            return Err(ApiError::unauthorized());
        }
    }
    token.ok_or(ApiError::unauthorized())
}
async fn authenticate(
    State(state): State<ApiState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = cookie_token(request.headers())?;
    let session = state.auth.session(token, unix_time()).await?;
    if !matches!(*request.method(), Method::GET | Method::HEAD)
        && single(request.headers(), "x-csrf-token") != Some(csrf_token(token)?.as_str())
    {
        return Err(ApiError::forbidden());
    }
    let _network_guard = if (request.uri().path() == "/api/deployment/network"
        && request.method() == Method::POST)
        || (request.uri().path() == "/api/users" && request.method() == Method::POST)
        || (request.uri().path().starts_with("/api/users/") && request.method() == Method::DELETE)
    {
        None
    } else {
        Some(state.network_gate.read().await)
    };
    request.extensions_mut().insert(session);
    Ok(next.run(request).await)
}
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
}
impl ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "login_required",
        }
    }
    fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "request_origin_or_csrf_rejected",
        }
    }
    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
        }
    }
    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "temporarily_unavailable",
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(json!({"error":self.code}))).into_response();
        if self.status == StatusCode::TOO_MANY_REQUESTS {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        }
        response
    }
}
impl From<ManagementError> for ApiError {
    fn from(error: ManagementError) -> Self {
        let (status, code) = match error {
            ManagementError::Unauthorized => (StatusCode::UNAUTHORIZED, "login_required"),
            ManagementError::Limited => (StatusCode::TOO_MANY_REQUESTS, "login_rate_limited"),
            ManagementError::Invalid => (StatusCode::BAD_REQUEST, "invalid_credentials_format"),
            ManagementError::Storage => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
            ManagementError::InvalidUsername => (StatusCode::BAD_REQUEST, "invalid_username"),
            ManagementError::InvalidPassword => (StatusCode::BAD_REQUEST, "invalid_password"),
            ManagementError::UsernameExists => (StatusCode::CONFLICT, "username_exists"),
            ManagementError::UserNotFound => (StatusCode::NOT_FOUND, "user_not_found"),
            ManagementError::TargetIsAdmin => (StatusCode::CONFLICT, "target_is_admin"),
            ManagementError::CurrentPasswordIncorrect => {
                (StatusCode::BAD_REQUEST, "current_password_incorrect")
            }
        };
        Self { status, code }
    }
}
impl From<ChainError> for ApiError {
    fn from(error: ChainError) -> Self {
        OperationError::from(error).into()
    }
}
impl From<OperationError> for ApiError {
    fn from(error: OperationError) -> Self {
        Self {
            status: StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code: error.code,
        }
    }
}
struct ApiJson<T>(T);
impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;
    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(request, state)
            .await
            .map_err(|_| ApiError {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_request",
            })?;
        Ok(Self(value))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: SecretString,
}
async fn login(
    State(state): State<ApiState>,
    ApiJson(body): ApiJson<LoginRequest>,
) -> Result<Response, ApiError> {
    let login = state
        .auth
        .login(&body.username, body.password, unix_time())
        .await?;
    let csrf = csrf_token(login.token.expose())?;
    let mut response = Json(json!({"identity":login.identity,"csrf":csrf})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        state.cookie(login.token.expose(), SESSION_SECONDS)?,
    );
    Ok(response)
}
async fn session(
    Extension(identity): Extension<SessionIdentity>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        json!({"identity":identity,"csrf":csrf_token(cookie_token(&headers)?)?}),
    ))
}
async fn logout(State(state): State<ApiState>, headers: HeaderMap) -> Result<Response, ApiError> {
    state.auth.logout(cookie_token(&headers)?).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, state.cookie("", 0)?);
    Ok(response)
}
async fn snapshot(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let controller = state.controller(&identity)?;
    let config = controller.configure(Command::Snapshot).await?;
    Ok(Json(
        // Bind data and CSRF to the same authenticated request, even if another tab changes cookies.
        json!({"configuration":config,"chains":controller.status().await?,"session":{"identity":identity,"csrf":csrf_token(cookie_token(&headers)?)?}}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountLogin {
    expected: Option<AccountIdentity>,
}
async fn begin_account(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    ApiJson(body): ApiJson<AccountLogin>,
) -> Result<Json<Reply>, ApiError> {
    Ok(Json(
        state
            .controller(&identity)?
            .configure(Command::BeginAccountLogin {
                port: 1455,
                expected: body.expected,
            })
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountRequest {
    account: AccountIdentity,
}
async fn check_account(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    ApiJson(body): ApiJson<AccountRequest>,
) -> Result<Json<Reply>, ApiError> {
    Ok(Json(
        state
            .controller(&identity)?
            .configure(Command::CheckAccount {
                account: body.account,
            })
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Callback {
    callback: SecretString,
}
async fn complete_login(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<Callback>,
) -> Result<Json<Reply>, ApiError> {
    Ok(Json(
        state
            .controller(&identity)?
            .configure(Command::CompleteLogin {
                transaction: id,
                callback: body.callback,
            })
            .await?,
    ))
}
async fn cancel_login(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<Json<Reply>, ApiError> {
    Ok(Json(
        state
            .controller(&identity)?
            .configure(Command::CancelLogin { transaction: id })
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewEndpoint {
    kind: EndpointKind,
    account: AccountIdentity,
    name: String,
    output: Option<Uuid>,
}
async fn create_endpoint(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    ApiJson(body): ApiJson<NewEndpoint>,
) -> Result<Json<Reply>, ApiError> {
    let command = match body.kind {
        EndpointKind::In => Command::CreateIn {
            account: body.account,
            name: body.name,
            output: body.output,
        },
        EndpointKind::Out => Command::CreateOut {
            account: body.account,
            name: body.name,
        },
    };
    Ok(Json(state.controller(&identity)?.configure(command).await?))
}
async fn delete_endpoint(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<Json<bridge_core::lifecycle::DeleteOutcome>, ApiError> {
    Ok(Json(state.controller(&identity)?.delete(id).await?))
}
async fn begin_out(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<Json<Reply>, ApiError> {
    Ok(Json(
        state
            .controller(&identity)?
            .configure(Command::BeginOutLogin {
                port: 1455,
                output: id,
            })
            .await?,
    ))
}
async fn recover_out(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<Json<Reply>, ApiError> {
    Ok(Json(
        state
            .controller(&identity)?
            .configure(Command::RecoverOut { output: id })
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreRecordRequest {
    client_id: String,
}
async fn restore_out_record(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<RestoreRecordRequest>,
) -> Result<Json<Reply>, ApiError> {
    Ok(Json(
        state
            .controller(&identity)?
            .configure(Command::RestoreOutRecord {
                output: id,
                client_id: body.client_id,
            })
            .await
            .map_err(operation_error::restore_record)?,
    ))
}
async fn sync_output_device(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.controller(&identity)?.sync_output_device(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn sync_input_device(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.controller(&identity)?.sync_input_device(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn pair_in(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<Json<Reply>, ApiError> {
    Ok(Json(
        state
            .controller(&identity)?
            .configure(Command::PairIn { input: id })
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairRequest {
    code: SecretString,
}
async fn pair_out(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<PairRequest>,
) -> Result<Json<Reply>, ApiError> {
    Ok(Json(
        state
            .controller(&identity)?
            .configure(Command::PairOut {
                output: id,
                code: body.code,
            })
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingRequest {
    output: Uuid,
}
async fn bind(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<BindingRequest>,
) -> Result<Json<Reply>, ApiError> {
    Ok(Json(
        state
            .controller(&identity)?
            .configure(Command::Bind {
                input: id,
                output: body.output,
            })
            .await?,
    ))
}
async fn unbind(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.controller(&identity)?.unbind(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn start(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.controller(&identity)?.start(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn stop(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.controller(&identity)?.stop(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn switch(
    State(state): State<ApiState>,
    Extension(identity): Extension<SessionIdentity>,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<BindingRequest>,
) -> Result<StatusCode, ApiError> {
    state.controller(&identity)?.switch(id, body.output).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod authorization_error_tests {
    use super::*;
    use bridge_core::{
        auth::AuthError, configuration::ConfigurationError, enrollment::EnrollmentError,
        upstream::ControlError,
    };
    #[test]
    fn authorization_failures_have_distinct_safe_codes() {
        for (error, expected) in [
            (AuthError::Network, "upstream_auth_network"),
            (AuthError::Rejected, "upstream_auth_rejected"),
            (AuthError::Identity, "upstream_auth_identity"),
            (AuthError::Transaction, "upstream_auth_transaction"),
            (AuthError::Callback, "upstream_auth_callback"),
            (AuthError::Declined, "upstream_auth_declined"),
            (AuthError::Response, "upstream_auth_response"),
            (AuthError::StepUp, "device_authorization_required"),
        ] {
            let mapped = ApiError::from(ChainError::Configuration(ConfigurationError::Auth(error)));
            assert_eq!(mapped.code, expected);
            assert_eq!(mapped.status, StatusCode::CONFLICT);
        }
        let mapped = ApiError::from(ChainError::Configuration(ConfigurationError::Auth(
            AuthError::Storage,
        )));
        assert_eq!(mapped.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn shared_failures_preserve_http_status_and_json_shape() {
        let repair = operation_error::restore_record(ChainError::Configuration(
            ConfigurationError::Enrollment(EnrollmentError::Control(ControlError::Registration)),
        ));
        for (error, status, code) in [
            (
                ApiError::from(ChainError::Closed),
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
            ),
            (
                ApiError::from(ChainError::Configuration(ConfigurationError::Auth(
                    AuthError::Storage,
                ))),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
            (
                ApiError::from(repair),
                StatusCode::CONFLICT,
                "device_authorization_required",
            ),
        ] {
            let response = error.into_response();
            assert_eq!(response.status(), status);
            let bytes = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                json!({"error": code}),
            );
        }
    }
}
