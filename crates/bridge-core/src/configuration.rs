//! Tenant configuration commands run inside the same actor as cutover and deletion.
//! Replies contain user-facing metadata or one-time login/pairing values, never long-term tokens.
use crate::{
    auth::{AuthError, LoginPurpose, LoginResult, LoginStart, SecretString},
    chains::RuntimeFactory,
    identity, lifecycle,
    model::{AccountIdentity, Binding, Endpoint, EndpointKind, Target},
    runtime::{BridgeRuntime, RuntimeError},
    storage::StorageError,
    upstream::{ControlError, unix_time},
    vault::{SecretScope, VaultError},
};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

pub enum Command {
    Snapshot,
    CreateOut {
        account: AccountIdentity,
        name: String,
    },
    CreateIn {
        account: AccountIdentity,
        name: String,
        output: Option<Uuid>,
    },
    BeginAccountLogin {
        expected: Option<AccountIdentity>,
        port: u16,
    },
    RecoverOut {
        output: Uuid,
    },
    RestoreOutRecord {
        output: Uuid,
        client_id: String,
    },
    BeginOutLogin {
        output: Uuid,
        port: u16,
    },
    CompleteLogin {
        transaction: Uuid,
        callback: SecretString,
    },
    CancelLogin {
        transaction: Uuid,
    },
    CheckAccount {
        account: AccountIdentity,
    },
    PairIn {
        input: Uuid,
    },
    PairOut {
        output: Uuid,
        code: SecretString,
    },
    Bind {
        input: Uuid,
        output: Uuid,
    },
}
#[derive(Serialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum Reply {
    Done,
    Snapshot(Snapshot),
    Endpoint(Endpoint),
    Account(AccountIdentity),
    Login(LoginStart),
    Pairing { code: SecretString, expires_at: i64 },
    PairedOutput(crate::output_pairing::PairedOutput),
}
#[derive(Serialize)]
pub struct Snapshot {
    pub accounts: Vec<AccountInfo>,
    pub endpoints: Vec<EndpointInfo>,
}
#[derive(Serialize)]
pub struct AccountInfo {
    pub sign_in_required: bool,
    #[serde(flatten)]
    pub account: AccountIdentity,
    #[serde(flatten)]
    pub profile: crate::auth::AccountProfile,
}
#[derive(Serialize)]
pub struct EndpointInfo {
    pub endpoint: Endpoint,
    pub lifecycle: String,
    pub registered: bool,
    pub target: Option<Target>,
    /// Display-only name of the controlled device, when the upstream listing reported one.
    pub target_name: Option<String>,
    pub target_online: Option<bool>,
    pub target_last_seen_at: Option<i64>,
    /// An unfinished pairing hides the prior device; OUT pairing also blocks connections.
    pub pairing_pending: bool,
    pub paired_device: Option<crate::input_pairing::PairedDevice>,
    pub pairing_error: Option<String>,
    pub registration_uncertain: bool,
    pub registration_recoverable: bool,
}
#[derive(Debug, thiserror::Error)]
pub enum ConfigurationError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Enrollment(#[from] crate::enrollment::EnrollmentError),
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Lifecycle(#[from] lifecycle::DeleteError),
    #[error(transparent)]
    PairOutput(#[from] crate::output_pairing::PairOutputError),
    #[error(transparent)]
    PairInput(#[from] crate::input_pairing::PairInputError),
    #[error("configuration is unavailable or requires cleanup")]
    State,
}
impl RuntimeFactory {
    pub(crate) async fn sync_output_device(
        &self,
        tenant: Uuid,
        output: Uuid,
    ) -> Result<bool, ConfigurationError> {
        let endpoint = self.usable(tenant, output, EndpointKind::Out).await?;
        crate::output_sync::sync(self, &endpoint, unix_time()).await
    }
    pub(crate) async fn sync_input_device(
        &self,
        tenant: Uuid,
        input: Uuid,
    ) -> Result<bool, ConfigurationError> {
        let endpoint = self.usable(tenant, input, EndpointKind::In).await?;
        crate::input_sync::sync(self, &endpoint, unix_time()).await
    }
    async fn usable(
        &self,
        tenant: Uuid,
        device: Uuid,
        kind: EndpointKind,
    ) -> Result<Endpoint, ConfigurationError> {
        let endpoint = self
            .store
            .endpoint(tenant, device)
            .await?
            .ok_or(ConfigurationError::State)?;
        if endpoint.kind != kind || self.store.endpoint_deleting(tenant, device).await? {
            return Err(ConfigurationError::State);
        }
        Ok(endpoint)
    }
    pub(crate) async fn configure(
        &self,
        tenant: Uuid,
        command: Command,
    ) -> Result<Reply, ConfigurationError> {
        match command {
            Command::Snapshot => {
                let identities=sqlx::query("SELECT workspace_id,user_id FROM accounts WHERE tenant_id=? ORDER BY workspace_id,user_id")
                    .bind(tenant.to_string()).fetch_all(&self.store.pool).await.map_err(StorageError::from)?
                    .into_iter().map(|row|Ok(AccountIdentity {workspace_id:row.try_get("workspace_id")?,user_id:row.try_get("user_id")?})).collect::<Result<Vec<_>,sqlx::Error>>().map_err(StorageError::from)?;
                let mut accounts = Vec::with_capacity(identities.len());
                for account in identities {
                    let profile = self.auth.cached_profile(tenant, &account).await?;
                    accounts.push(AccountInfo {
                        sign_in_required: self.store.sign_in_required(tenant, &account).await?,
                        account,
                        profile,
                    });
                }
                let rows=sqlx::query("SELECT device_id,lifecycle FROM endpoints WHERE tenant_id=? ORDER BY kind,identity_name")
                    .bind(tenant.to_string()).fetch_all(&self.store.pool).await.map_err(StorageError::from)?;
                let mut endpoints = Vec::new();
                for row in rows {
                    let id: Uuid = row
                        .try_get::<String, _>("device_id")
                        .map_err(StorageError::from)?
                        .parse()
                        .map_err(|_| ConfigurationError::State)?;
                    let endpoint = self
                        .store
                        .endpoint(tenant, id)
                        .await?
                        .ok_or(ConfigurationError::State)?;
                    let scope = match endpoint.kind {
                        EndpointKind::In => SecretScope::InRegistration(id),
                        EndpointKind::Out => SecretScope::OutRegistration(id),
                    };
                    let registered = self.vault.read(tenant, &scope).await?.is_some();
                    let registration_uncertain = endpoint.kind == EndpointKind::Out
                        && self.store.enrollment_pending(tenant, id).await?;
                    let input_pairing = if endpoint.kind == EndpointKind::In {
                        crate::input_pairing::snapshot(&self.store, tenant, id).await?
                    } else {
                        crate::input_pairing::PairingState::default()
                    };
                    let computer = self.store.target_record(tenant, id).await?;
                    endpoints.push(EndpointInfo {
                        endpoint: endpoint.clone(),
                        lifecycle: row.try_get("lifecycle").map_err(StorageError::from)?,
                        registered,
                        target: self.store.target(tenant, id).await?,
                        target_name: computer.name,
                        target_online: computer.online,
                        target_last_seen_at: computer.last_seen_at,
                        pairing_pending: if endpoint.kind == EndpointKind::In {
                            input_pairing.pending
                        } else {
                            self.store.output_pairing_pending(tenant, id).await?
                        },
                        paired_device: input_pairing.device,
                        pairing_error: if endpoint.kind == EndpointKind::In {
                            input_pairing.error
                        } else {
                            computer.error
                        },
                        registration_uncertain,
                        registration_recoverable: registration_uncertain
                            && endpoint.kind == EndpointKind::Out
                            && self
                                .vault
                                .read(tenant, &SecretScope::OutEnrollment(id))
                                .await?
                                .is_some(),
                    });
                }
                return Ok(Reply::Snapshot(Snapshot {
                    accounts,
                    endpoints,
                }));
            }
            Command::CreateOut { account, name } => {
                self.auth
                    .account(tenant, &account, None, unix_time())
                    .await?;
                let seed = self.vault.tenant_seed(tenant).await?;
                let endpoint = identity::endpoint(&seed, tenant, account, &name, EndpointKind::Out)
                    .map_err(|_| ConfigurationError::State)?;
                self.store.create_endpoint(&endpoint).await?;
                return Ok(Reply::Endpoint(endpoint));
            }
            Command::CreateIn {
                account,
                name,
                output,
            } => {
                let version = if let Some(output) = output {
                    self.usable(tenant, output, EndpointKind::Out).await?;
                    self.store
                        .target(tenant, output)
                        .await?
                        .ok_or(ConfigurationError::State)?
                        .app_server_version
                } else {
                    format!("codex-rc-bridge/{}", env!("CARGO_PKG_VERSION"))
                };
                self.auth
                    .account(tenant, &account, None, unix_time())
                    .await?;
                let seed = self.vault.tenant_seed(tenant).await?;
                let endpoint = identity::endpoint(&seed, tenant, account, &name, EndpointKind::In)
                    .map_err(|_| ConfigurationError::State)?;
                self.store.create_endpoint(&endpoint).await?;
                self.upstream
                    .enroll_host(&self.auth, &self.vault, &endpoint, &version)
                    .await?;
                return Ok(Reply::Endpoint(endpoint));
            }
            Command::BeginAccountLogin { expected, port } => {
                return Ok(Reply::Login(
                    self.auth
                        .begin(
                            tenant,
                            LoginPurpose::Account { expected },
                            port,
                            unix_time(),
                        )
                        .await?,
                ));
            }
            Command::RestoreOutRecord { output, client_id } => {
                let endpoint = self
                    .store
                    .endpoint(tenant, output)
                    .await?
                    .ok_or(ConfigurationError::State)?;
                crate::enrollment::restore_controller_record(
                    &self.vault,
                    &self.auth,
                    &self.upstream,
                    &endpoint,
                    &client_id,
                )
                .await?;
            }
            Command::RecoverOut { output } => {
                let endpoint = self.usable(tenant, output, EndpointKind::Out).await?;
                crate::enrollment::recover_controller(
                    &self.vault,
                    &self.auth,
                    &self.upstream,
                    &endpoint,
                )
                .await?;
            }
            Command::BeginOutLogin { output, port } => {
                let endpoint = self.usable(tenant, output, EndpointKind::Out).await?;
                let uncertain:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM enrollment_attempts WHERE tenant_id=? AND device_id=?)")
                    .bind(tenant.to_string()).bind(output.to_string()).fetch_one(&self.store.pool).await.map_err(StorageError::from)?;
                if uncertain {
                    return Err(ConfigurationError::State);
                }
                return Ok(Reply::Login(
                    self.auth
                        .begin(
                            tenant,
                            LoginPurpose::OutStepUp {
                                device_id: output,
                                account: endpoint.account,
                            },
                            port,
                            unix_time(),
                        )
                        .await?,
                ));
            }
            Command::CompleteLogin {
                transaction,
                callback,
            } => {
                match self
                    .auth
                    .complete(tenant, transaction, callback.expose(), unix_time())
                    .await?
                {
                    LoginResult::Account(tokens) => {
                        return Ok(Reply::Account(tokens.account.clone()));
                    }
                    LoginResult::StepUp {
                        device_id,
                        token,
                        account,
                    } => {
                        let endpoint = self.usable(tenant, device_id, EndpointKind::Out).await?;
                        if endpoint.account != account {
                            return Err(ConfigurationError::State);
                        }
                        crate::enrollment::register_controller(
                            &self.vault,
                            &self.auth,
                            &self.upstream,
                            &endpoint,
                            &token,
                        )
                        .await?;
                        return Ok(Reply::Endpoint(endpoint));
                    }
                }
            }
            Command::CancelLogin { transaction } => {
                let changed =
                    sqlx::query("DELETE FROM oauth_transactions WHERE tenant_id=? AND id=?")
                        .bind(tenant.to_string())
                        .bind(transaction.to_string())
                        .execute(&self.store.pool)
                        .await
                        .map_err(StorageError::from)?
                        .rows_affected();
                if changed != 1 {
                    return Err(ConfigurationError::State);
                }
            }
            Command::CheckAccount { account } => {
                self.upstream
                    .check_account(&self.auth, tenant, &account)
                    .await?;
            }
            Command::PairIn { input } => {
                let endpoint = self.usable(tenant, input, EndpointKind::In).await?;
                let version = match self.store.binding(tenant, input).await? {
                    Some(binding) => {
                        self.store
                            .target(tenant, binding.out_id)
                            .await?
                            .ok_or(ConfigurationError::State)?
                            .app_server_version
                    }
                    None => format!("codex-rc-bridge/{}", env!("CARGO_PKG_VERSION")),
                };
                let session = self
                    .upstream
                    .registered_host_session(&self.auth, &self.vault, &endpoint, &version, None)
                    .await?;
                crate::input_pairing::begin(self, &endpoint, &session.environment_id).await?;
                let pairing =
                    lifecycle::pair_host(&self.store, &self.upstream, tenant, input, &session)
                        .await?;
                let expires_at = pairing.expires_unix()?;
                crate::input_pairing::issued(&self.store, tenant, input, expires_at).await?;
                return Ok(Reply::Pairing {
                    code: pairing.manual_pairing_code,
                    expires_at,
                });
            }
            Command::PairOut { output, code } => {
                return Ok(Reply::PairedOutput(
                    crate::output_pairing::pair(
                        &self.vault,
                        &self.auth,
                        &self.upstream,
                        tenant,
                        output,
                        &code,
                    )
                    .await?,
                ));
            }
            Command::Bind { input, output } => {
                let binding = Binding {
                    tenant_id: tenant,
                    in_id: input,
                    out_id: output,
                };
                // Prepare validates active roles, registration/key ownership and local loops; no socket opens.
                drop(
                    BridgeRuntime::load_binding(
                        self.store.clone(),
                        self.vault.clone(),
                        self.auth.clone(),
                        self.upstream.clone(),
                        binding.clone(),
                    )
                    .await?,
                );
                self.store.bind(&binding).await?;
            }
        }
        Ok(Reply::Done)
    }
}
