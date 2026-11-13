//! Guided development setup; no long-lived credentials or callbacks are printed.
use bridge_core::{
    auth::{AuthService, LoginPurpose, LoginResult, SecretString},
    chains::{ChainFactory, RuntimeFactory},
    configuration::{Command, Reply},
    device::Registration,
    identity,
    model::{AccountIdentity, Binding, EndpointKind},
    storage::SqliteStore,
    upstream::{Upstream, unix_time},
    vault::{SecretScope, Vault},
};
use std::{
    io::{self, Write},
    sync::Arc,
};
use uuid::Uuid;
use zeroize::Zeroizing;
type Error = Box<dyn std::error::Error>;

pub async fn configure(
    store: &Arc<SqliteStore>,
    vault: &Vault,
    auth: &Arc<AuthService<Upstream>>,
    upstream: &Upstream,
    tenant: Uuid,
) -> Result<(Uuid, PairingTracker), Error> {
    println!(
        "配置一条测试链路。授权链接、回调和配对码仅供本次操作，请勿把此向导输出重定向到日志。"
    );
    println!("\n1/6 登录 IN 的账号：应与手机控制端 A 使用的账号一致。");
    let input_account = login_account(auth, tenant).await?;
    println!("\n2/6 登录 OUT 的账号：应与被控电脑 B 上 Codex 使用的账号一致。");
    let output_account = login_account(auth, tenant).await?;
    let name = prompt(
        "OUT 固定身份名 [desktop-controller]：",
        Some("desktop-controller"),
    )?;
    let seed = vault.tenant_seed(tenant).await?;
    let output = identity::endpoint(
        &seed,
        tenant,
        output_account.clone(),
        &name,
        EndpointKind::Out,
    )?;
    if store.endpoint(tenant, output.device_id).await?.is_none() {
        store.create_endpoint(&output).await?;
    }
    if store.endpoint_deleting(tenant, output.device_id).await? {
        return Err("此 OUT 正在删除，须先完成清理".into());
    }
    println!("\n3/6 完成 OUT 的远控二次授权。");
    let _registration: Registration = match vault
        .read(tenant, &SecretScope::OutRegistration(output.device_id))
        .await?
    {
        Some(raw) => serde_json::from_slice(&raw)?,
        None => {
            let result = authorize(
                auth,
                tenant,
                LoginPurpose::OutStepUp {
                    device_id: output.device_id,
                    account: output_account.clone(),
                },
            )
            .await?;
            let LoginResult::StepUp { token, .. } = result else {
                return Err("二次授权事务类型不匹配".into());
            };
            bridge_core::enrollment::register_controller(vault, auth, upstream, &output, &token)
                .await?
        }
    };
    println!("\n4/6 配对被控电脑 B，自动保存唯一目标。");
    let target = if let Some(target) = store.target(tenant, output.device_id).await?
        && !store
            .output_pairing_pending(tenant, output.device_id)
            .await?
    {
        println!("沿用此 OUT 已保存的目标。");
        target
    } else {
        let code = SecretString::new(rpassword::prompt_password(
            "在 B 上开启远控，然后输入 B 显示的配对码（不回显）：",
        )?);
        bridge_core::output_pairing::pair(vault, auth, upstream, tenant, output.device_id, &code)
            .await?;
        store
            .target(tenant, output.device_id)
            .await?
            .ok_or("配对后目标未保存")?
    };
    if store.is_local_environment(&target.environment_id).await? {
        return Err("不能把本部署的虚拟 IN 当作 B".into());
    }
    println!("\n5/6 创建 IN，并与此 OUT 绑定。");
    let name = prompt("IN 固定身份名 [phone-entry]：", Some("phone-entry"))?;
    let input = identity::endpoint(&seed, tenant, input_account, &name, EndpointKind::In)?;
    if store.endpoint(tenant, input.device_id).await?.is_none() {
        store.create_endpoint(&input).await?;
    }
    if let Some(binding) = store.binding(tenant, input.device_id).await?
        && binding.out_id != output.device_id
    {
        return Err("此 IN 已绑定其他 OUT；向导不会直接覆盖绑定".into());
    }
    upstream
        .enroll_host(auth, vault, &input, &target.app_server_version)
        .await?;
    if store.binding(tenant, input.device_id).await?.is_none() {
        store
            .bind(&Binding {
                tenant_id: tenant,
                in_id: input.device_id,
                out_id: output.device_id,
            })
            .await?;
    }
    println!("\n6/6 在控制设备的 ChatGPT 中配对。");
    let tracker = pair_input(store, vault, auth, upstream, tenant, input.device_id).await?;
    if let Err(error) = prompt("完成配对后按回车启动桥接：", Some("")) {
        tracker.stop().await?;
        return Err(error);
    }
    println!(
        "IN：{}\nOUT：{}\n桥接将持续运行；进入 IN 后即可验证目标对话。按 Ctrl+C 正常停止。",
        input.device_id, output.device_id
    );
    Ok((input.device_id, tracker))
}

/// The CLI owns the tracker for the complete code lifetime, just like the coordinator.
pub struct PairingTracker {
    stop: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl PairingTracker {
    pub async fn stop(self) -> Result<(), Error> {
        self.stop.cancel();
        self.task.await?;
        Ok(())
    }
    pub async fn wait(mut self) -> Result<(), Error> {
        tokio::select! {
            result = &mut self.task => result?,
            signal = tokio::signal::ctrl_c() => {
                signal?;
                self.stop.cancel();
                self.task.await?;
            }
        }
        Ok(())
    }
}
pub async fn pair_input(
    store: &Arc<SqliteStore>,
    vault: &Vault,
    auth: &Arc<AuthService<Upstream>>,
    upstream: &Upstream,
    tenant: Uuid,
    input: Uuid,
) -> Result<PairingTracker, Error> {
    let factory = RuntimeFactory {
        store: store.clone(),
        vault: vault.clone(),
        auth: auth.clone(),
        upstream: upstream.clone(),
    };
    let Reply::Pairing { code, expires_at } =
        factory.configure(tenant, Command::PairIn { input }).await?
    else {
        return Err("配对命令回复不符".into());
    };
    println!("在控制设备的 ChatGPT 中输入短期配对码：{}", code.expose());
    println!("配对跟踪持续至配对码过期；按 Ctrl+C 停止。");
    let stop = tokio_util::sync::CancellationToken::new();
    let (changes, _) = tokio::sync::watch::channel(());
    let task = tokio::spawn(bridge_core::input_pairing::follow(
        factory,
        tenant,
        input,
        expires_at,
        stop.clone(),
        changes,
    ));
    Ok(PairingTracker { stop, task })
}

async fn login_account(
    auth: &AuthService<Upstream>,
    tenant: Uuid,
) -> Result<AccountIdentity, Error> {
    match authorize(auth, tenant, LoginPurpose::Account { expected: None }).await? {
        LoginResult::Account(tokens) => Ok(tokens.account),
        _ => Err("普通登录事务类型不匹配".into()),
    }
}
async fn authorize(
    auth: &AuthService<Upstream>,
    tenant: Uuid,
    purpose: LoginPurpose,
) -> Result<LoginResult, Error> {
    let transaction = auth.begin(tenant, purpose, 1455, unix_time()).await?;
    println!(
        "在自己的浏览器打开：\n{}\n完成授权后，即使 localhost 页面打不开，也请复制地址栏完整地址。",
        transaction.url
    );
    let callback = Zeroizing::new(rpassword::prompt_password("粘贴回调地址（不回显）：")?);
    Ok(auth
        .complete(tenant, transaction.id, callback.trim(), unix_time())
        .await?)
}
fn prompt(message: &str, default: Option<&str>) -> Result<String, Error> {
    print!("{message}");
    io::stdout().flush()?;
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer)? == 0 {
        return Err("输入已结束，配置未完成".into());
    }
    let answer = answer.trim_end_matches(['\r', '\n']);
    Ok(if answer.is_empty() {
        default.unwrap_or("").to_owned()
    } else {
        answer.to_owned()
    })
}

/// Local deployment operator, before any upstream authorization.
pub async fn configure_network(vault: &Vault) -> Result<(), Error> {
    println!("部署网络：直接回车使用直连；或输入 HTTP 代理地址，例如 http://127.0.0.1:7890。");
    let value = SecretString::new(rpassword::prompt_password(
        "代理地址（可含认证信息，输入不回显）：",
    )?);
    let proxy = if value.expose().is_empty() {
        None
    } else {
        Some(&value)
    };
    Upstream::new(proxy.map(SecretString::expose))
        .map_err(|_| "代理地址无效，仅支持 http://主机:端口")?;
    bridge_core::deployment::save(vault, proxy).await?;
    println!("部署网络设置已加密保存；之后可由部署管理员在网页设置中修改。");
    Ok(())
}
