use crate::service::{DesktopService, Error};
use serde_json::Value;
use sqlx::sqlite::SqliteConnectOptions;
use std::{
    fs::{File, OpenOptions},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

struct Application {
    service: Arc<DesktopService>,
    exiting: AtomicBool,
    _database_lock: File,
}
#[tauri::command]
async fn desktop_request(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Application>,
    path: String,
    method: String,
    body: Option<Value>,
) -> Result<Value, Error> {
    if window.label() != "main" || state.exiting.load(Ordering::SeqCst) {
        return Err(Error::local(
            "The application is exiting or the window is unavailable.",
        ));
    }
    let service = state.service.clone();
    // The accepted operation owns its lifetime independently of webview reloads.
    tauri::async_runtime::spawn(async move {
        service
            .request(path, method, body.unwrap_or(Value::Null))
            .await
    })
    .await
    .map_err(|_| Error::local("The desktop operation did not complete."))?
}
#[tauri::command]
fn desktop_open_authorization(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    url: String,
) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|_| "Invalid authorization link.")?;
    if window.label() != "main"
        || parsed.scheme() != "https"
        || parsed.host_str() != Some("auth.openai.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("The authorization link must use the official authorization service.".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|_| "Cannot open the system browser. Copy the authorization link instead.".into())
}
fn tray_menu(
    app: &tauri::AppHandle,
    chinese: bool,
) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{Menu, MenuItem};
    let open = MenuItem::with_id(
        app,
        "open",
        if chinese {
            "打开 Codex RC Bridge"
        } else {
            "Open Codex RC Bridge"
        },
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        if chinese {
            "退出并停止桥接"
        } else {
            "Quit and stop bridging"
        },
        true,
        None::<&str>,
    )?;
    Menu::with_items(app, &[&open, &quit])
}
#[tauri::command]
fn desktop_set_language(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    language: String,
) -> Result<(), String> {
    if window.label() != "main" || !matches!(language.as_str(), "en" | "zh") {
        return Err("Invalid language request".into());
    }
    #[cfg(target_os = "macos")]
    {
        let menu = tauri::menu::Menu::default(&app).map_err(|e| e.to_string())?;
        if language == "zh" {
            localize_native_menu(menu.items().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        }
        app.set_menu(menu).map_err(|e| e.to_string())?;
    }
    if let Some(tray) = app.tray_by_id("bridge") {
        tray.set_menu(Some(
            tray_menu(&app, language == "zh").map_err(|e| e.to_string())?,
        ))
        .map_err(|e| e.to_string())?;
        tray.set_tooltip(Some("Codex RC Bridge"))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[cfg(target_os = "macos")]
fn localize_native_menu(items: Vec<tauri::menu::MenuItemKind<tauri::Wry>>) -> tauri::Result<()> {
    use tauri::menu::MenuItemKind;
    fn label(text: &str) -> String {
        let plain = text.replace('&', "");
        let translated = match plain.as_str() {
            "File" => "文件",
            "Edit" => "编辑",
            "View" => "显示",
            "Window" => "窗口",
            "Help" => "帮助",
            "Undo" => "撤销",
            "Redo" => "重做",
            "Cut" => "剪切",
            "Copy" => "复制",
            "Paste" => "粘贴",
            "Select All" => "全选",
            "Minimize" => "最小化",
            "Zoom" => "缩放",
            "Close" | "Close Window" => "关闭窗口",
            "Toggle Full Screen" => "切换全屏",
            "Hide Others" => "隐藏其他",
            "Show All" => "显示全部",
            "Services" => "服务",
            "About" => "关于",
            "Hide" => "隐藏",
            "Quit" => "退出",
            _ => {
                for (prefix, translated) in
                    [("About ", "关于 "), ("Hide ", "隐藏 "), ("Quit ", "退出 ")]
                {
                    if let Some(name) = plain.strip_prefix(prefix) {
                        return format!("{translated}{name}");
                    }
                }
                return text.to_owned();
            }
        };
        translated.to_owned()
    }
    for item in items {
        match item {
            MenuItemKind::Submenu(item) => {
                item.set_text(label(&item.text()?))?;
                localize_native_menu(item.items()?)?;
            }
            MenuItemKind::Predefined(item) => item.set_text(label(&item.text()?))?,
            MenuItemKind::MenuItem(item) => item.set_text(label(&item.text()?))?,
            _ => {}
        }
    }
    Ok(())
}
fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        #[cfg(target_os = "macos")]
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
fn exit(app: &tauri::AppHandle) {
    let state = app.state::<Application>();
    if state.exiting.swap(true, Ordering::SeqCst) {
        return;
    }
    let service = state.service.clone();
    let app = app.clone();
    let _ = app.emit("desktop-exiting", ());
    tauri::async_runtime::spawn(async move {
        let result = service.shutdown().await;
        app.exit(if result.is_ok() { 0 } else { 1 });
    });
}
fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let path = app.path().app_local_data_dir()?;
    std::fs::create_dir_all(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    let database = path.join("bridge.sqlite");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(database.with_extension("admin-lock"))?;
    lock.try_lock()
        .map_err(|_| "Another process is using the desktop configuration.")?;
    let key = crate::credentials::load(database.exists())?;
    let service = tauri::async_runtime::block_on(async {
        let store = bridge_core::storage::SqliteStore::open(
            SqliteConnectOptions::new()
                .filename(database)
                .create_if_missing(true),
        )
        .await
        .map_err(|_| "Cannot open the desktop database.".to_owned())?;
        DesktopService::open(Arc::new(store), key)
            .await
            .map_err(|e| e.message)
    })?;
    let mut changes = service.controller.subscribe();
    let stop = service.stop.clone();
    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                result = changes.changed() => {
                    if result.is_err() { break; }
                    let _ = handle.emit("bridge-changed", ());
                }
            }
        }
    });
    app.manage(Application {
        service,
        exiting: AtomicBool::new(false),
        _database_lock: lock,
    });
    let menu = tray_menu(app.handle(), false)?;
    tauri::tray::TrayIconBuilder::with_id("bridge")
        .icon(tauri::include_image!("icons/tray.png"))
        .icon_as_template(true)
        .tooltip("Codex RC Bridge")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show(app),
            "quit" => exit(app),
            _ => {}
        })
        .build(app)?;
    Ok(())
}
pub fn run() {
    let application = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            desktop_request,
            desktop_open_authorization,
            desktop_set_language
        ])
        .setup(setup)
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if window.hide().is_ok() {
                    #[cfg(target_os = "macos")]
                    let _ = window
                        .app_handle()
                        .set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("Cannot start the application. Check Keychain and local configuration.");
    application.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { api, code, .. } if code.is_none() => {
            api.prevent_exit();
            exit(app);
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => show(app),
        _ => {}
    });
}
