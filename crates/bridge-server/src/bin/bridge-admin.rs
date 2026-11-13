//! Local operator-only bootstrap. Never bind management APIs before management authentication exists.
use bridge_server::key_file;
#[path = "../manage_chains.rs"]
mod manage_chains;
#[path = "../setup_chain.rs"]
mod setup_chain;
use bridge_core::{
    auth::{AuthService, LoginPurpose, LoginResult, SecretString},
    device::Registration,
    identity,
    model::{AccountIdentity, Binding, EndpointKind},
    runtime::{BridgeRuntime, load_registration},
    storage::SqliteStore,
    upstream::{HostRegistration, Upstream, unix_time},
    vault::{SecretScope, Vault},
};
use sqlx::sqlite::SqliteConnectOptions;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;
use zeroize::Zeroizing;

type Error = Box<dyn std::error::Error>;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Error> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let automatic = args == ["setup"] || args == ["start"] || args == ["manage"];
    let starting = args == ["start"];
    let managing = args == ["manage"];
    if automatic {
        for path in ["local-state", "secrets"] {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            if !Path::new(path).try_exists()? {
                builder.create(path)?;
            }
        }
        args = vec![
            "local-state/bridge.sqlite".into(),
            "secrets/master.key".into(),
            if managing {
                "manage".into()
            } else if starting {
                "start-local".into()
            } else {
                "setup-chain".into()
            },
        ];
    }
    if args.len() < 3 {
        eprintln!(
            "本地向导：bridge-admin setup\n显式配置：bridge-admin <数据库> <独立主密钥文件> <命令>\n部署配置：configure-proxy <tenant> | grant-admin <tenant> <username>\n命令：init | setup-chain <tenant> | login <tenant> | complete <tenant> <transaction>\nauthorize-out <tenant> <workspace> <user> <固定名称> | check-account <tenant> <workspace> <user>\npair-out <tenant> <out> | targets <tenant> <out>\ncreate-in <tenant> <workspace> <user> <固定名称> <out> | pair-in <tenant> <in> | bind <tenant> <in> <out> | run <tenant> <in>"
        );
        eprintln!(
            "管理初始化：new-tenant | add-user <tenant> <username> | reset-password <tenant> <username>\n多链路控制台：manage <tenant>\n恢复注册：recover-out <tenant> <out>\n补回注册记录：restore-out-record <tenant> <out> <原client_id>\n离线读取备份编号：show-out-record <tenant> <out>"
        );
        return Err("缺少参数".into());
    }
    let db = PathBuf::from(&args[0]);
    let key = PathBuf::from(&args[1]);
    let command = args[2].as_str();
    let initialize =
        command == "init" || (automatic && !starting && !managing && !db.try_exists()?);
    // Explicit bootstrap paths: the key must not be stored in the database directory/backup tree.
    let db_parent = parent(&db)?.canonicalize()?;
    let key_parent = parent(&key)?.canonicalize()?;
    if key_parent.starts_with(&db_parent) {
        return Err("请将主密钥放在数据库目录之外，并分别备份".into());
    }
    let _lock = key_file::lock_database(&db)?;
    if initialize {
        if args.len() != 3 || db.try_exists()? {
            return Err("初始化要求数据库尚不存在；不会覆盖已有状态".into());
        }
        key_file::create(&key)?;
    }
    let kek = key_file::load(&key)?;
    let store = Arc::new(
        SqliteStore::open(
            SqliteConnectOptions::new()
                .filename(&db)
                .create_if_missing(initialize),
        )
        .await?,
    );
    let vault = Vault::new(store.clone(), kek);
    if command == "new-tenant" && args.len() == 3 {
        let tenant = Uuid::new_v4();
        vault.create_tenant_with_seed(tenant).await?;
        println!("Tenant：{tenant}");
        return Ok(());
    }
    if initialize {
        let tenant = Uuid::new_v4();
        vault.create_tenant_with_seed(tenant).await?;
        println!("初始化完成。Tenant：{tenant}");
        if command == "init" {
            return Ok(());
        }
    }
    let tenant: Uuid = if automatic {
        let tenants = store.local_tenants().await?;
        if tenants.len() != 1 {
            return Err(
                "默认向导要求本地状态中只有一个租户；多租户部署请使用显式 setup-chain <tenant>"
                    .into(),
            );
        }
        tenants[0]
    } else {
        args.get(3).ok_or("缺少 tenant")?.parse()?
    };
    // Validate KEK and tenant before any authorization/network effects.
    let seed = vault
        .read(tenant, &SecretScope::TenantSeed)
        .await?
        .ok_or("租户不存在或未初始化")?;
    if seed.len() != 32 {
        return Err("租户种子无效".into());
    }
    // Offline inspection of a backup copy: no upstream client, refresh or coordinator is created.
    if command == "show-out-record" && args.len() == 5 {
        let device = args[4].parse()?;
        let endpoint = store.endpoint(tenant, device).await?.ok_or("OUT 不存在")?;
        if endpoint.kind != EndpointKind::Out {
            return Err("只能读取 OUT 注册编号".into());
        }
        let mut record = None;
        for scope in [
            SecretScope::OutRegistration(device),
            SecretScope::OutEnrollment(device),
        ] {
            if let Some(raw) = vault.read(tenant, &scope).await? {
                record = Some(serde_json::from_slice::<bridge_core::device::Registration>(
                    &raw,
                )?);
                break;
            }
        }
        let record = record.ok_or("备份中没有此 OUT 的注册记录")?;
        let seed = bridge_core::identity::TenantSeed::from_bytes(seed.as_slice().try_into()?);
        let derived = bridge_core::identity::endpoint(
            &seed,
            tenant,
            endpoint.account.clone(),
            &endpoint.identity_name,
            EndpointKind::Out,
        )?;
        let key = bridge_core::identity::signing_key(
            &seed,
            tenant,
            &endpoint.account,
            &endpoint.identity_name,
        )?;
        if derived.device_id != device
            || bridge_core::device::identity(&key).key_id != record.key_id
        {
            return Err("注册记录与备份中的原设备身份不匹配".into());
        }
        bridge_core::identity::validate_field(&record.client_id)?;
        println!("原注册 client ID：{}", record.client_id);
        return Ok(());
    }
    if command == "grant-admin" && args.len() == 5 {
        bridge_core::management::ManagementAuth::new(store.clone())
            .grant_deployment_admin(tenant, &args[4])
            .await?;
        println!("已授予部署管理员权限。");
        return Ok(());
    }
    if command == "configure-proxy" && args.len() == 4 {
        setup_chain::configure_network(&vault).await?;
        return Ok(());
    }
    if command == "add-user"
        && args.len() == 5
        && bridge_core::deployment::load(&vault).await?.is_none()
    {
        setup_chain::configure_network(&vault).await?;
    }
    if matches!(command, "add-user" | "reset-password") && args.len() == 5 {
        let password =
            SecretString::new(rpassword::prompt_password("管理密码（至少 12 个字符）：")?);
        let repeat = SecretString::new(rpassword::prompt_password("再次输入：")?);
        if password.expose() != repeat.expose() {
            return Err("两次密码不一致".into());
        }
        let management = bridge_core::management::ManagementAuth::new(store.clone());
        if command == "add-user" {
            management.create_user(tenant, &args[4], password).await?;
        } else {
            management
                .reset_password(tenant, &args[4], password)
                .await?;
        }
        println!("管理登录已更新。");
        return Ok(());
    }
    // Explicit per-process adapter setting; absence means direct. No implicit HTTP(S)_PROXY inheritance.
    let proxy =
        bridge_core::deployment::bootstrap(&vault, std::env::var("BRIDGE_UPSTREAM_PROXY").ok())
            .await?;
    if command == "setup-chain" && bridge_core::deployment::load(&vault).await?.is_none() {
        setup_chain::configure_network(&vault).await?;
    }
    let proxy = bridge_core::deployment::load(&vault)
        .await?
        .unwrap_or(proxy);
    let upstream = Upstream::new(proxy.as_ref().map(SecretString::expose))?;
    let auth = Arc::new(AuthService::new(vault.clone(), upstream.clone()));
    match command {
        "restore-out-record" if args.len() == 6 => {
            let device = args[4].parse()?;
            let endpoint = store.endpoint(tenant, device).await?.ok_or("OUT 不存在")?;
            bridge_core::enrollment::restore_controller_record(
                &vault, &auth, &upstream, &endpoint, &args[5],
            )
            .await?;
            println!(
                "已核验原公钥并补回注册记录；待删除出口继续保持停用。使用 recover-out 恢复草稿出口，或在管理入口重试清理。"
            );
        }
        "recover-out" if args.len() == 5 => {
            let device = args[4].parse()?;
            let endpoint = store.endpoint(tenant, device).await?.ok_or("OUT 不存在")?;
            bridge_core::enrollment::recover_controller(&vault, &auth, &upstream, &endpoint)
                .await?;
            println!("已使用原设备身份恢复 OUT 注册。");
        }
        "manage" if automatic || args.len() == 4 => {
            manage_chains::run(
                bridge_core::chains::RuntimeFactory {
                    store,
                    vault,
                    auth,
                    upstream,
                },
                tenant,
            )
            .await?;
        }
        "login" if args.len() == 4 => {
            show_login(
                auth.begin(
                    tenant,
                    LoginPurpose::Account { expected: None },
                    1455,
                    unix_time(),
                )
                .await?,
            );
        }
        "authorize-out" if args.len() == 7 => {
            let account = AccountIdentity {
                workspace_id: args[4].clone(),
                user_id: args[5].clone(),
            };
            auth.account(tenant, &account, None, unix_time()).await?;
            let seed = vault.tenant_seed(tenant).await?;
            let endpoint =
                identity::endpoint(&seed, tenant, account.clone(), &args[6], EndpointKind::Out)?;
            if vault
                .read(tenant, &SecretScope::OutRegistration(endpoint.device_id))
                .await?
                .is_some()
            {
                return Err("此 OUT 已注册；不重复创建上游注册".into());
            }
            if store.endpoint(tenant, endpoint.device_id).await?.is_none() {
                store.create_endpoint(&endpoint).await?;
            }
            println!("OUT：{}", endpoint.device_id);
            show_login(
                auth.begin(
                    tenant,
                    LoginPurpose::OutStepUp {
                        device_id: endpoint.device_id,
                        account,
                    },
                    1455,
                    unix_time(),
                )
                .await?,
            );
        }
        "complete" if args.len() == 5 => {
            let transaction = args[4].parse()?;
            let callback = Zeroizing::new(rpassword::prompt_password(
                "粘贴完整回调地址（输入不回显）：",
            )?);
            match auth
                .complete(tenant, transaction, callback.trim(), unix_time())
                .await?
            {
                LoginResult::Account(tokens) => println!(
                    "账号登录已加密保存。Workspace：{}，User：{}",
                    tokens.account.workspace_id, tokens.account.user_id
                ),
                LoginResult::StepUp {
                    device_id,
                    token,
                    account,
                } => {
                    let endpoint = store
                        .endpoint(tenant, device_id)
                        .await?
                        .ok_or("OUT 已不存在")?;
                    if endpoint.account != account || endpoint.kind != EndpointKind::Out {
                        return Err("OUT 身份不匹配".into());
                    }
                    if vault
                        .read(tenant, &SecretScope::OutRegistration(device_id))
                        .await?
                        .is_some()
                    {
                        return Err("OUT 已注册，此授权事务不再适用".into());
                    }
                    bridge_core::enrollment::register_controller(
                        &vault, &auth, &upstream, &endpoint, &token,
                    )
                    .await?;
                    println!(
                        "OUT 授权完成，固定设备密钥的上游注册已加密保存。尚未配对或启动桥接。"
                    );
                }
            }
        }
        "check-account" if args.len() == 6 => {
            let account = AccountIdentity {
                workspace_id: args[4].clone(),
                user_id: args[5].clone(),
            };
            upstream.check_account(&auth, tenant, &account).await?;
            println!("账号授权有效，必要的续期已保存。此检查不代表被控设备在线。");
        }
        "create-in" if args.len() == 8 => {
            let account = AccountIdentity {
                workspace_id: args[4].clone(),
                user_id: args[5].clone(),
            };
            let output: Uuid = args[7].parse()?;
            if store.endpoint_deleting(tenant, output).await? {
                return Err("目标 OUT 正在删除，不能为它注册新 IN".into());
            }
            let target = store
                .target(tenant, output)
                .await?
                .ok_or("先选择 OUT 的目标设备")?;
            auth.account(tenant, &account, None, unix_time()).await?;
            let seed = vault.tenant_seed(tenant).await?;
            let endpoint = identity::endpoint(&seed, tenant, account, &args[6], EndpointKind::In)?;
            if store.endpoint(tenant, endpoint.device_id).await?.is_none() {
                store.create_endpoint(&endpoint).await?;
            }
            upstream
                .enroll_host(&auth, &vault, &endpoint, &target.app_server_version)
                .await?;
            println!("IN 已注册：{}。尚未绑定或上线。", endpoint.device_id);
        }
        "pair-in" if args.len() == 5 => {
            let input: Uuid = args[4].parse()?;
            setup_chain::pair_input(&store, &vault, &auth, &upstream, tenant, input)
                .await?
                .wait()
                .await?;
        }
        "pair-out" | "targets" if args.len() == 5 => {
            let output: Uuid = args[4].parse()?;
            if command == "pair-out" {
                let code = SecretString::new(rpassword::prompt_password(
                    "输入 B 上生成的配对码（输入不回显）：",
                )?);
                let paired = bridge_core::output_pairing::pair(
                    &vault, &auth, &upstream, tenant, output, &code,
                )
                .await?;
                println!(
                    "配对完成，已保存被控设备：{} ({})",
                    paired.name.as_deref().unwrap_or("未命名设备"),
                    paired.environment_id
                );
            } else {
                let endpoint = store.endpoint(tenant, output).await?.ok_or("OUT 不存在")?;
                if endpoint.kind != EndpointKind::Out {
                    return Err("对象不是 OUT".into());
                }
                let registration: Registration =
                    load_registration(&vault, tenant, SecretScope::OutRegistration(output)).await?;
                for environment in upstream
                    .environments(&auth, tenant, &endpoint.account, &registration.client_id)
                    .await?
                {
                    println!(
                        "{}  {}",
                        environment.env_id,
                        environment.label().as_deref().unwrap_or("未命名设备")
                    );
                }
            }
        }

        "bind" if args.len() == 6 => {
            let input: Uuid = args[4].parse()?;
            let output: Uuid = args[5].parse()?;
            let _: HostRegistration =
                load_registration(&vault, tenant, SecretScope::InRegistration(input)).await?;
            let _: Registration =
                load_registration(&vault, tenant, SecretScope::OutRegistration(output)).await?;
            let target = store
                .target(tenant, output)
                .await?
                .ok_or("OUT 尚未选择目标")?;
            if store.is_local_environment(&target.environment_id).await? {
                return Err("禁止桥接到本部署的虚拟 IN".into());
            }
            store
                .bind(&Binding {
                    tenant_id: tenant,
                    in_id: input,
                    out_id: output,
                })
                .await?;
            println!("绑定已保存。离线和停止运行不会释放此独占关系。");
        }
        "run" | "start-local"
            if (command == "run" && args.len() == 5) || (command == "start-local" && starting) =>
        {
            let input: Uuid = if starting {
                let bindings = store.bindings(tenant).await?;
                if bindings.len() != 1 {
                    return Err("本地 start 要求只有一条绑定；请使用显式 run <tenant> <in>".into());
                }
                bindings[0].in_id
            } else {
                args[4].parse()?
            };
            run_chain(store, vault, auth, upstream, tenant, input).await?;
        }
        "setup-chain" if automatic || args.len() == 4 => {
            let (input, tracker) =
                setup_chain::configure(&store, &vault, &auth, &upstream, tenant).await?;
            let result = run_chain(store, vault, auth, upstream, tenant, input).await;
            tracker.stop().await?;
            result?;
        }
        _ => return Err("命令或参数数量无效".into()),
    }
    Ok(())
}
async fn run_chain(
    store: Arc<SqliteStore>,
    vault: Vault,
    auth: Arc<AuthService<Upstream>>,
    upstream: Upstream,
    tenant: Uuid,
    input: Uuid,
) -> Result<(), Error> {
    tracing_subscriber::fmt().with_target(false).init();
    let runtime = BridgeRuntime::load(store, vault, auth, upstream, tenant, input).await?;
    let stop = tokio_util::sync::CancellationToken::new();
    let running = runtime.run(stop.clone());
    tokio::pin!(running);
    tokio::select! {result=&mut running=>result?,signal=tokio::signal::ctrl_c()=>{signal?;println!("正在停止桥接并等待凭据操作完成…");stop.cancel();running.await?;}}
    Ok(())
}
fn parent(path: &Path) -> Result<&Path, Error> {
    Ok(path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new(".")))
}
fn show_login(login: bridge_core::auth::LoginStart) {
    println!(
        "授权事务：{}\n请在自己的浏览器打开以下链接。授权后 localhost 页面可能无法打开；复制地址栏的完整回调地址，用 complete 提交。事务有效期 10 分钟。\n{}",
        login.id, login.url
    );
}
