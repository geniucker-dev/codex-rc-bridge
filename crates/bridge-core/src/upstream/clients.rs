use super::*;

#[derive(Deserialize, Serialize)]
pub struct PairedClient {
    pub client_id: String,
    pub display_name: Option<String>,
    pub platform: Option<String>,
    pub device_model: Option<String>,
    pub device_type: Option<String>,
    pub last_seen_at: Option<String>,
}
#[derive(Deserialize)]
struct ClientPage {
    items: Vec<PairedClient>,
    cursor: Option<String>,
}

fn path(environment: Option<&str>, client: Option<&str>) -> Result<String, ControlError> {
    let mut url = url::Url::parse("https://chatgpt.com/wham/remote/control/")
        .map_err(|_| ControlError::Invalid)?;
    {
        let mut parts = url.path_segments_mut().map_err(|_| ControlError::Invalid)?;
        parts.pop_if_empty();
        if let Some(environment) = environment {
            validate_id(environment)?;
            parts.push("environments").push(environment);
        }
        parts.push("clients");
        if let Some(client) = client {
            validate_id(client)?;
            parts.push(client);
        }
    }
    Ok(url.path().to_owned())
}
fn validate_id(value: &str) -> Result<(), ControlError> {
    crate::identity::validate_field(value).map_err(|_| ControlError::Invalid)?;
    if matches!(value, "." | "..") {
        return Err(ControlError::Invalid);
    }
    Ok(())
}
impl Upstream {
    /// Validate cached/refreshed credentials with an authenticated, read-only upstream request.
    pub async fn check_account<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
    ) -> Result<(), ControlError> {
        let revision = auth.begin_check(tenant, account).await?;
        let _: ClientPage = self
            .account_request(
                auth,
                tenant,
                account,
                ControlRequest {
                    method: reqwest::Method::GET,
                    path: "/wham/remote/control/clients?limit=1&order=desc",
                    body: None,
                    installation: None,
                    refresh_start: false,
                },
            )
            .await?;
        auth.confirm_check(tenant, account, revision).await?;
        Ok(())
    }
    async fn clients<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        environment: Option<&str>,
    ) -> Result<Vec<PairedClient>, ControlError> {
        let base = path(environment, None)?;
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        let mut result = Vec::new();
        for _ in 0..100 {
            let mut url = url::Url::parse(&format!("https://chatgpt.com{base}"))
                .map_err(|_| ControlError::Invalid)?;
            url.query_pairs_mut()
                .append_pair("limit", "100")
                .append_pair("order", "desc");
            if let Some(cursor) = cursor.as_deref() {
                url.query_pairs_mut().append_pair("cursor", cursor);
            }
            let request_path = format!(
                "{}?{}",
                url.path(),
                url.query().ok_or(ControlError::Invalid)?
            );
            let page: ClientPage = self
                .account_request(
                    auth,
                    tenant,
                    account,
                    ControlRequest {
                        method: reqwest::Method::GET,
                        path: &request_path,
                        body: None,
                        installation: None,
                        refresh_start: false,
                    },
                )
                .await?;
            if result.len() + page.items.len() > 10000 {
                return Err(ControlError::Invalid);
            }
            for item in page.items {
                validate_id(&item.client_id)?;
                if ids.insert(item.client_id.clone()) {
                    result.push(item);
                }
            }
            match page.cursor {
                None => return Ok(result),
                Some(next) if next.is_empty() => return Ok(result),
                Some(next) => {
                    if next.len() > 8192 || !seen.insert(next.clone()) {
                        return Err(ControlError::Invalid);
                    }
                    cursor = Some(next);
                }
            }
        }
        Err(ControlError::Invalid)
    }
    pub(crate) async fn environment_clients<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        environment: &str,
    ) -> Result<Vec<String>, ControlError> {
        Ok(self
            .environment_client_details(auth, tenant, account, environment)
            .await?
            .into_iter()
            .map(|client| client.client_id)
            .collect())
    }
    /// Revoke only this environment, retaining its identity until cleanup completes.
    pub(crate) async fn revoke_environment_clients<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        environment: &str,
    ) -> Result<bool, ControlError> {
        let clients = self
            .environment_clients(auth, tenant, account, environment)
            .await?;
        // Bound work per attempt. A later attempt continues the same environment.
        let partial = clients.len() > 32;
        for client in clients.into_iter().take(32) {
            self.revoke_environment_client(auth, tenant, account, environment, &client)
                .await?;
        }
        Ok(!partial
            && self
                .environment_clients(auth, tenant, account, environment)
                .await?
                .is_empty())
    }
    pub(crate) async fn environment_client_details<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        environment: &str,
    ) -> Result<Vec<PairedClient>, ControlError> {
        self.clients(auth, tenant, account, Some(environment)).await
    }
    async fn revoke_client<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        environment: Option<&str>,
        client: &str,
    ) -> Result<(), ControlError> {
        let path = path(environment, Some(client))?;
        let result: Result<serde_json::Value, ControlError> = self
            .account_request(
                auth,
                tenant,
                account,
                ControlRequest {
                    method: reqwest::Method::DELETE,
                    path: &path,
                    body: None,
                    installation: None,
                    refresh_start: false,
                },
            )
            .await;
        match result {
            Ok(_) => Ok(()),
            // A lost success response may make retry return 404. Confirm absence through
            // the same scope; never interpret an arbitrary 404 or auth failure as success.
            Err(ControlError::Rejected(404))
                if !self
                    .clients(auth, tenant, account, environment)
                    .await?
                    .iter()
                    .any(|item| item.client_id == client) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    /// Clear only this controller's environment grants, not its device registration.
    pub(crate) async fn clear_controller_environments<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        client: &str,
    ) -> Result<(), ControlError> {
        let environments = self.environments(auth, tenant, account, client).await?;
        for environment in environments {
            let path = path(Some(&environment.env_id), Some(client))?;
            let result: Result<serde_json::Value, ControlError> = self
                .account_request(
                    auth,
                    tenant,
                    account,
                    ControlRequest {
                        method: reqwest::Method::DELETE,
                        path: &path,
                        body: None,
                        installation: None,
                        refresh_start: false,
                    },
                )
                .await;
            if let Err(error) = result {
                // A missing environment or a lost reply is settled by the OUT's
                // own access list. Never delete another client or the global identity.
                if self
                    .environments(auth, tenant, account, client)
                    .await?
                    .iter()
                    .any(|e| e.env_id == environment.env_id)
                {
                    return Err(error);
                }
            }
        }
        if !self
            .environments(auth, tenant, account, client)
            .await?
            .is_empty()
        {
            return Err(ControlError::Invalid);
        }
        Ok(())
    }
    pub(crate) async fn revoke_controller<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        client: &str,
    ) -> Result<(), ControlError> {
        self.revoke_client(auth, tenant, account, None, client)
            .await
    }
    pub(crate) async fn revoke_environment_client<T: TokenEndpoint>(
        &self,
        auth: &AuthService<T>,
        tenant: Uuid,
        account: &AccountIdentity,
        environment: &str,
        client: &str,
    ) -> Result<(), ControlError> {
        self.revoke_client(auth, tenant, account, Some(environment), client)
            .await
    }
}
