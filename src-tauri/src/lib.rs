mod codebuddy_oauth;
mod cursor_bridge;
mod gateway;
mod models;
mod update;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, RunEvent,
};

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "tray-open", "打开 CodeRelay", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "tray-start", "启动反代服务", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "tray-stop", "停止反代服务", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray-quit", "退出 CodeRelay", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &start, &stop, &quit])?;

    gateway::register_tray_items(app.handle(), start.clone(), stop.clone());

    TrayIconBuilder::with_id("coderelay-tray")
        .icon(
            app.default_window_icon()
                .cloned()
                .expect("missing default window icon"),
        )
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("CodeRelay")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray-open" => show_main_window(app),
            "tray-start" => {
                let _ = gateway::start_service_for_tray(app);
            }
            "tray-stop" => {
                let _ = gateway::stop_service_for_tray(app);
            }
            "tray-quit" => gateway::quit_from_tray(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn run() {
    let app = tauri::Builder::default()
        // 单实例锁必须是第一个注册的插件：再次启动 CodeRelay 时不会新开窗口，
        // 而是聚焦已有主窗口，避免多实例并发操作 sidecar 引发端口冲突与状态错乱。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(gateway::RuntimeState::new())
        .manage(cursor_bridge::CursorBridgeState::new())
        .setup(|app| {
            let runtime = app.state::<gateway::RuntimeState>().inner().clone();
            gateway::initialize(app.handle(), &runtime)?;
            let bridge = app.state::<cursor_bridge::CursorBridgeState>();
            cursor_bridge::initialize(app.handle(), &bridge)?;
            drop(bridge);
            build_tray(app)?;
            gateway::sync_tray_menu(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            gateway::get_app_state,
            gateway::save_service_config,
            gateway::save_accounts,
            gateway::save_api_keys,
            gateway::export_accounts,
            gateway::clear_request_logs,
            gateway::refresh_account_quota,
            gateway::refresh_all_quotas,
            gateway::codebuddy_checkin_status,
            gateway::codebuddy_checkin,
            gateway::start_service,
            gateway::stop_service,
            codebuddy_oauth::codebuddy_oauth_start,
            codebuddy_oauth::codebuddy_oauth_complete,
            codebuddy_oauth::codebuddy_oauth_cancel,
            codebuddy_oauth::codebuddy_validate_token,
            cursor_bridge::cursor_bridge_status,
            cursor_bridge::cursor_bridge_start,
            cursor_bridge::cursor_bridge_stop,
            cursor_bridge::cursor_bridge_init_ca,
            cursor_bridge::cursor_bridge_install_command,
            cursor_bridge::cursor_bridge_set_enabled,
            cursor_bridge::cursor_bridge_sync_models,
            cursor_bridge::cursor_bridge_save_bindings,
            cursor_bridge::cursor_bridge_save_preferences,
            update::check_for_update,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build CodeRelay");

    app.run(|app_handle, event| {
        if matches!(event, RunEvent::Exit) {
            let runtime = app_handle.state::<gateway::RuntimeState>().inner().clone();
            gateway::shutdown(app_handle, &runtime);
            let bridge = app_handle.state::<cursor_bridge::CursorBridgeState>();
            cursor_bridge::shutdown(&bridge);
        }
    });
}
