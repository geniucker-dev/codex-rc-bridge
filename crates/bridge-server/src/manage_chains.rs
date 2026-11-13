//! Local console adapter. No management socket or credentials are exposed.
use bridge_core::chains::{ChainController, ChainCoordinator, ChainStatus, RuntimeFactory};
use bridge_core::lifecycle::DeleteOutcome;
use std::io::BufRead;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub async fn run(factory: RuntimeFactory, tenant: Uuid) -> Result<(), super::Error> {
    tracing_subscriber::fmt().with_target(false).init();
    let stop = CancellationToken::new();
    let (control, mut worker) =
        ChainCoordinator::spawn(factory.store.clone(), factory, tenant, stop.clone());
    let (sender, mut lines) = tokio::sync::mpsc::channel(16);
    // A dedicated stdin thread does not hold Tokio's blocking pool open on Ctrl+C.
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            if sender.blocking_send(line).is_err() {
                break;
            }
        }
    });
    println!(
        "多链路本地控制台。已启用链路会自动恢复；运行中不代表目标在线。\n命令：list | start <IN> | stop <IN> | switch <IN> <OUT> | unbind <IN> | delete <IN或OUT> | recover <OUT> | restore-record <OUT> <原client_id> | quit\nquit / Ctrl+C 停止本进程的全部链路并保留启用设置；stop 会保存暂停设置。delete 会撤销此端点的远端权限，待清理状态下再次 delete 可重试。"
    );
    let signal = tokio::signal::ctrl_c();
    tokio::pin!(signal);
    let result = loop {
        let line = tokio::select! {
            result=&mut worker=>return result?.map_err(Into::into),
            signal=&mut signal=>break signal.map_err(Into::into),
            line=lines.recv()=>match line {
                Some(Ok(line))=>line,
                Some(Err(error))=>break Err(error.into()),
                None=>break Ok(()),
            }
        };
        if line.trim() == "quit" {
            break Ok(());
        }
        let result = tokio::select! {
            signal=&mut signal=>break signal.map_err(Into::into),
            result=command(&control,&line)=>result,
        };
        if let Err(error) = result {
            eprintln!("操作失败：{error}");
        }
    };
    println!("正在停止全部链路并等待凭据操作完成…");
    stop.cancel();
    worker.await??;
    result
}

async fn command(control: &ChainController, line: &str) -> Result<(), super::Error> {
    let fields: Vec<_> = line.split_whitespace().collect();
    match fields.as_slice() {
        ["list"] => {
            for endpoint in control.deleting().await? {
                println!("{} {}：删除待完成（已停用，使用 delete 重试）",endpoint.kind.as_str(),endpoint.device_id);
            }
            for info in control.status().await? {
                println!(
                    "IN {} → OUT {}：{}（{}）",
                    info.binding.in_id,
                    info.binding.out_id,
                    match info.status {
                        ChainStatus::Stopped => "已停止",
                        ChainStatus::Running => "运行中",
                        ChainStatus::Failed => "运行失败",
                        ChainStatus::Switching => "切换待恢复",
                    },
                    if info.enabled {
                        "已启用"
                    } else {
                        "已暂停"
                    }
                );
            }
        }
        ["start", input] => {
            control.start(input.parse()?).await?;
            println!("已启用此链路，正在确认目标在线并连接。");
        }
        ["stop", input] => {
            control.stop(input.parse()?).await?;
            println!("此链路已停止，绑定保留。");
        }
        ["switch", input, output] => {
            control.switch(input.parse()?, output.parse()?).await?;
            println!("绑定已切换。已启用链路将连接新目标；手机可能需要退出设备页面后重新进入。");
        }
        ["unbind", input] => {
            control.unbind(input.parse()?).await?;
            println!("此链路已停止并解绑，两侧身份和授权保留。");
        }
        ["delete", device] => {
            match control.delete(device.parse()?).await? {
                DeleteOutcome::Complete=>println!("远端撤权和本地清理已完成；同名身份可重新创建并授权。"),
                DeleteOutcome::RegistrationUnknown=>println!("已停用；先前注册的远端结果无法确认，保留待清理状态，不能直接删除或同名重建。"),
                DeleteOutcome::RevocationPending=>println!("已停用，远端配对尚未全部清理；再次 delete 继续。"),
            }
        }
        ["restore-record", output, client_id] => {
            control.configure(bridge_core::configuration::Command::RestoreOutRecord { output: output.parse()?, client_id: (*client_id).into() }).await?;
            println!("已核验原公钥并补回注册记录。草稿出口可使用 recover；待删除出口继续保持停用，使用 delete 重试撤权。");
        }
        ["recover", output] => {
            control.configure(bridge_core::configuration::Command::RecoverOut { output: output.parse()? }).await?;
            println!("已使用原身份验证并恢复 OUT 注册；未重新注册或更换密钥。");
        }
        [] => {}
        _ => return Err(
            "命令无效；使用 list / start <IN> / stop <IN> / switch <IN> <OUT> / unbind <IN> / delete <IN或OUT> / recover <OUT> / quit"
                .into(),
        ),
    }
    Ok(())
}
